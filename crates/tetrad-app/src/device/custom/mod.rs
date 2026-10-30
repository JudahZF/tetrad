//! Custom footswitch mode (owned, Tetrad): the footswitch mode an extension declares in its
//! manifest (`footswitchMode`, API 4), run by Tetrad (`tetrad_ui::custom_mode`).
//!
//! The mode is a layer over the stored stock mode (`FootswitchContext::custom`): Cortex
//! Control and MIDI CC#47 still see the stock mode, and leaving Custom returns to it. While it
//! is on, the ten switches A–D, Down, E–H and Tap are the slots of the current page and Bank Up
//! is the mode switch (tetrad-core `FootswitchContext::press`).
//!
//! A press captures what its release must do, so a release after a page change still ends the
//! action it started (a momentary bypass comes back). Leaving the mode, a new layout of a held
//! slot and a new provider release every held slot; a preset load drops the held momentary
//! bypasses, which belong to the old preset's blocks.
//!
//! A switch can also have a double press, and two adjacent switches a combo. Footswitch presses
//! (and MIDI CC#35..=42, which are footswitch edges) go through `gesture::Gestures`, which
//! decides which of them runs; Gig View taps and Mappings presses run the press at once. A page change, leaving the mode, a preset load and a
//! layout edit drop a press that still waits.
//!
//! The ALL PRESETS layout is one per extension installation, in [`LAYOUT_DIR`]; removing the
//! extension keeps it. A preset can have its own layout (THIS PRESET) in the preset field
//! [`TETRAD_FOOTSWITCH_FIELD`]; it plays instead while that preset is loaded, and its edits are
//! preset edits that the preset saves. [`ACTIVE_FILE`] names the installation whose mode was on,
//! so the mode comes back after a restart once the extension store is read.

mod gesture;

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tetrad_core::performance::leds::{CustomLed, LedBlock, PALETTE, block_colour};
use tetrad_engine::Site;
use tetrad_preset::model::TETRAD_FOOTSWITCH_FIELD;
use tetrad_ui::{
    BlockTarget, CUSTOM_SLOTS, CommandChoice, CustomComboView, CustomCommand, CustomModeView,
    CustomPageView, CustomSlotView, Gesture, LayoutScope, MidiMessageKind, MidiOutMessage,
    PageStep, SLOT_NAMES, Screen, SlotAction, SlotEdit, SystemAction, adjacent,
};

use self::gesture::{Fire, Gestures, Shape};

use super::mappings::{self, StoredTarget, SystemDef};
use super::preset::PresetEdit;
use super::{Action, Effect, State};

/// The installation whose mode was on at the last change.
pub const ACTIVE_FILE: &str = "/media/p4/tetrad/footswitch-mode.json";
/// One layout per installation: `<install key file stem>.json`.
pub const LAYOUT_DIR: &str = "/media/p4/tetrad/footswitch-modes";
/// Largest layout or active file. Read them with `read_bounded(.., MAX_FILE)`.
pub const MAX_FILE: u64 = 256 * 1024;
const FORMAT: u32 = 1;
/// Longest typed slot label.
pub const MAX_LABEL: usize = 16;
/// Messages of one Send MIDI slot, as a Preset MIDI Out list.
const MAX_MESSAGES: usize = tetrad_ui::MIDI_OUT_MESSAGES;
const PAGES: usize = tetrad_ui::CUSTOM_PAGES as usize;
const WHITE: [u8; 3] = [255; 3];

/// Where a press comes from. A slot held by one origin ignores the presses of the others, and
/// only the same origin releases it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Footswitch,
    /// A Mappings trigger (MIDI, an extension trigger).
    Mapping,
    /// A Gig View tile.
    Touch,
}

/// The installation that provides the mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    /// `owner/repo:extension.id`.
    pub key: String,
    /// The key as a file name (`InstallKey::file_stem`).
    pub stem: String,
    /// The extension's name.
    pub extension: String,
    /// The mode's name.
    pub name: String,
    pub pages: u8,
}

impl Provider {
    pub fn layout_path(&self) -> String {
        format!("{LAYOUT_DIR}/{}.json", self.stem)
    }
}

// ---- stored form ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum StoredAction {
    System {
        #[serde(with = "SystemDef")]
        action: SystemAction,
    },
    /// The block at a grid position; `model` (parameters only) detects another block there.
    Block {
        row: u8,
        column: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<u32>,
        target: StoredTarget,
    },
    /// A library file, found again in its setlist slot at each press; `name` is its label.
    LoadPreset {
        path: String,
        name: String,
    },
    SendMidi,
    Command {
        key: String,
        command: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum StoredKind {
    Cc,
    CcToggle,
    Pc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredMessage {
    kind: StoredKind,
    channel: u8,
    values: [Option<u8>; 3],
}

/// One action with its settings: the press of a switch, its double press or a combo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Binding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action: Option<StoredAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    /// The value of a parameter action, in percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<u8>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    momentary: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    messages: Vec<StoredMessage>,
}

impl Binding {
    fn valid(&self) -> bool {
        let label = (self.label.as_ref())
            .is_none_or(|label| (1..=MAX_LABEL).contains(&label.chars().count()));
        let action = match &self.action {
            Some(StoredAction::System { action }) => system_allowed(*action),
            Some(StoredAction::Block { row, column, .. }) => {
                mappings::position(*row, *column).is_some()
            }
            _ => true,
        };
        label
            && action
            && self.value.is_none_or(|value| value <= 100)
            && self.messages.len() <= MAX_MESSAGES
            && self.messages.iter().all(|message| {
                (1..=16).contains(&message.channel)
                    && message.values.iter().flatten().all(|v| *v < 128)
            })
    }
}

/// One switch: its press (stored flat, as before double presses), its colour and its double
/// press.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Slot {
    #[serde(flatten)]
    press: Binding,
    /// Index into the eight LED colours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    colour: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    double: Option<Binding>,
}

impl Slot {
    fn valid(&self) -> bool {
        self.press.valid()
            && self
                .colour
                .is_none_or(|colour| usize::from(colour) < PALETTE.len())
            && self.double.as_ref().is_none_or(Binding::valid)
    }
}

/// Two adjacent switches pressed together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Combo {
    /// Lower first.
    switches: [u8; 2],
    #[serde(flatten)]
    binding: Binding,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Page {
    slots: [Slot; CUSTOM_SLOTS],
    /// At most one per pair, each with an action.
    combos: Vec<Combo>,
}

impl Page {
    fn combo(&self, a: u8, b: u8) -> Option<&Combo> {
        let pair = [a.min(b), a.max(b)];
        self.combos.iter().find(|combo| combo.switches == pair)
    }

    /// The action `gesture` of `slot` runs; `None` without one (or with no such switch).
    fn binding(&self, slot: u8, gesture: Gesture) -> Option<&Binding> {
        let switch = self.slots.get(usize::from(slot))?;
        match gesture {
            Gesture::Press => Some(&switch.press),
            Gesture::Double => switch.double.as_ref(),
            Gesture::Combo(partner) => self.combo(slot, partner).map(|combo| &combo.binding),
        }
    }

    /// The binding to edit; `create` makes a missing double press or combo.
    fn binding_mut(&mut self, slot: usize, gesture: Gesture, create: bool) -> Option<&mut Binding> {
        match gesture {
            Gesture::Press => Some(&mut self.slots[slot].press),
            Gesture::Double if create => Some(self.slots[slot].double.get_or_insert_default()),
            Gesture::Double => self.slots[slot].double.as_mut(),
            Gesture::Combo(partner) => {
                let pair = [(slot as u8).min(partner), (slot as u8).max(partner)];
                let at = self.combos.iter().position(|combo| combo.switches == pair);
                let at = match at {
                    Some(at) => at,
                    None if create => {
                        self.combos.push(Combo {
                            switches: pair,
                            binding: Binding::default(),
                        });
                        self.combos.len() - 1
                    }
                    None => return None,
                };
                Some(&mut self.combos[at].binding)
            }
        }
    }

    /// What each switch has beyond its press, for [`Gestures`].
    fn shapes(&self) -> [Shape; CUSTOM_SLOTS] {
        let mut shapes = std::array::from_fn(|index| Shape {
            double: (self.slots[index].double.as_ref()).is_some_and(|d| d.action.is_some()),
            partners: 0,
        });
        for combo in &self.combos {
            let [a, b] = combo.switches.map(usize::from);
            shapes[a].partners |= 1 << b;
            shapes[b].partners |= 1 << a;
        }
        shapes
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEntry {
    page: u8,
    slot: u8,
    #[serde(flatten)]
    slot_data: Slot,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredCombo {
    page: u8,
    #[serde(flatten)]
    combo: Combo,
}

/// [`Provider::layout_path`]: the assigned slots and the combos. A Tetrad without combos and
/// double presses ignores those fields.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredLayout {
    format: u32,
    slots: Vec<StoredEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    combos: Vec<StoredCombo>,
}

/// [`ACTIVE_FILE`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredActive {
    format: u32,
    active: Option<String>,
}

/// A press that is down: what its release does.
#[derive(Debug, Clone, PartialEq)]
struct Held {
    origin: Origin,
    /// The page of the press: an edit of that page's slot ends it.
    page: u8,
    /// A combo is held on its lower switch; the first release of its two switches ends it.
    gesture: Gesture,
    release: Vec<Effect>,
    /// A momentary bypass: the release inverts this block's bypass as it is then, as a stock
    /// momentary release does, so overlapping presses of one block end where they started.
    invert: Option<Site>,
}

/// What a command asks the device to do.
#[derive(Debug, Default)]
pub struct Step {
    pub effects: Vec<Effect>,
    /// New contents of the provider's layout file.
    pub save: Option<(String, Vec<u8>)>,
}

#[derive(Debug, Default)]
pub struct CustomMode {
    provider: Option<Provider>,
    /// ALL PRESETS: always [`PAGES`] pages; the view shows the provider's.
    layout: Vec<Page>,
    /// THIS PRESET: the loaded preset's own layout, decoded at `preset_revision` ([`sync`]).
    preset_layout: Option<Vec<Page>>,
    preset_revision: Option<u64>,
    page: u8,
    held: [Option<Held>; CUSTOM_SLOTS],
    /// Footswitch presses that wait for a second press or a combo partner.
    gestures: Gestures,
    /// (page, slot, gesture) whose CC Toggle messages send the low value next. A combo is
    /// keyed on its lower switch.
    toggled: HashSet<(u8, u8, Gesture)>,
    last_tap: Option<Duration>,
    /// Commands of the running extensions, for the Command action.
    commands: Vec<CommandChoice>,
    notice: Option<String>,
    /// [`ACTIVE_FILE`]: the installation whose mode was on.
    restore: Option<String>,
}

impl CustomMode {
    pub fn provider(&self) -> Option<&Provider> {
        self.provider.as_ref()
    }

    pub fn page(&self) -> u8 {
        self.page
    }

    /// [`ACTIVE_FILE`] at start-up. A bad file is reported and ignored.
    pub fn load_active(&mut self, bytes: Option<&[u8]>) -> Option<String> {
        let bytes = bytes?;
        match serde_json::from_slice::<StoredActive>(bytes) {
            Ok(stored) if stored.format == FORMAT => {
                self.restore = stored.active;
                None
            }
            Ok(stored) => Some(format!(
                "{ACTIVE_FILE}: format {} is unknown",
                stored.format
            )),
            Err(error) => Some(format!("{ACTIVE_FILE}: {error}")),
        }
    }

    /// The contents of [`ACTIVE_FILE`] for the mode's state now.
    pub fn active_file(&self, active: bool) -> Vec<u8> {
        let stored = StoredActive {
            format: FORMAT,
            active: (self.provider.as_ref())
                .filter(|_| active)
                .map(|provider| provider.key.clone()),
        };
        serde_json::to_vec_pretty(&stored).expect("the active file encodes")
    }

    pub fn set_commands(&mut self, commands: Vec<CommandChoice>) {
        self.commands = commands;
    }

    /// The layout the switches play: the preset's own, else ALL PRESETS.
    fn playing(&self) -> &[Page] {
        self.preset_layout.as_deref().unwrap_or(&self.layout)
    }

    fn playing_scope(&self) -> LayoutScope {
        match self.preset_layout {
            Some(_) => LayoutScope::Preset,
            None => LayoutScope::Global,
        }
    }

    /// A slot of the playing layout.
    fn slot(&self, page: u8, slot: usize) -> Option<&Slot> {
        self.playing().get(usize::from(page))?.slots.get(slot)
    }

    /// When a waiting footswitch press is due: call [`expire`] then.
    pub fn deadline(&self) -> Option<Duration> {
        self.gestures.deadline()
    }
}

/// The stored form of `layout` (the layout file and the preset field).
fn encode(layout: &[Page]) -> Vec<u8> {
    let slots = (0..)
        .zip(layout)
        .flat_map(|(page, data)| {
            (0..)
                .zip(&data.slots)
                .filter(|(_, data)| **data != Slot::default())
                .map(move |(slot, data)| StoredEntry {
                    page,
                    slot,
                    slot_data: data.clone(),
                })
        })
        .collect();
    let combos = (0..)
        .zip(layout)
        .flat_map(|(page, data)| {
            (data.combos.iter()).map(move |combo| StoredCombo {
                page,
                combo: combo.clone(),
            })
        })
        .collect();
    serde_json::to_vec_pretty(&StoredLayout {
        format: FORMAT,
        slots,
        combos,
    })
    .expect("a layout encodes")
}

fn empty_layout() -> Vec<Page> {
    (0..PAGES).map(|_| Page::default()).collect()
}

/// The stored layout; bad entries are dropped and reported once.
fn decode(bytes: &[u8]) -> Result<(Vec<Page>, usize), String> {
    let stored: StoredLayout = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if stored.format != FORMAT {
        return Err(format!("format {} is unknown", stored.format));
    }
    let mut layout = empty_layout();
    let mut dropped = 0;
    for entry in stored.slots {
        let target = (layout.get_mut(usize::from(entry.page)))
            .and_then(|page| page.slots.get_mut(usize::from(entry.slot)));
        match target {
            Some(slot) if entry.slot_data.valid() => *slot = entry.slot_data,
            _ => dropped += 1,
        }
    }
    for StoredCombo { page, mut combo } in stored.combos {
        combo.switches.sort_unstable();
        let [a, b] = combo.switches;
        let valid = adjacent(a, b) && combo.binding.action.is_some() && combo.binding.valid();
        match layout.get_mut(usize::from(page)) {
            Some(page) if valid && page.combo(a, b).is_none() => page.combos.push(combo),
            _ => dropped += 1,
        }
    }
    Ok((layout, dropped))
}

/// System actions a slot may hold: every Mappings action but a slot press, with pages and
/// scenes in range.
fn system_allowed(action: SystemAction) -> bool {
    match action {
        SystemAction::CustomSlot(_) => false,
        SystemAction::CustomPage(PageStep::Go(page)) => usize::from(page) < PAGES,
        SystemAction::Scene(scene) => scene < 8,
        _ => true,
    }
}

/// Decodes the loaded preset's own layout again after any change of the preset (a load, an
/// edit, undo). A field that does not decode leaves the preset on the ALL PRESETS layout.
pub fn sync(state: &mut State) {
    let revision = state.current.revision();
    let custom = &mut state.custom;
    if custom.preset_revision == Some(revision) {
        return;
    }
    custom.preset_revision = Some(revision);
    let layout = (state.current.as_ref())
        .and_then(|current| current.preset.unknown.bytes_field(TETRAD_FOOTSWITCH_FIELD))
        .and_then(|bytes| decode(&bytes).ok())
        .map(|(layout, _)| layout);
    // A waiting press belongs to the layout it was pressed on.
    if layout != custom.preset_layout {
        custom.gestures.clear();
        custom.preset_layout = layout;
    }
}

// ---- mode and provider ----------------------------------------------------------------------

/// The installation that provides the mode now (the first enabled one with a footswitch mode,
/// by key), with its stored layout (`None`: no file). A new provider releases the held slots
/// and starts on page 1; a mode that is on goes off when its provider goes.
pub fn set_provider(
    state: &mut State,
    provider: Option<Provider>,
    layout: Option<Result<Vec<u8>, String>>,
) -> Vec<Effect> {
    let custom = &mut state.custom;
    let same = custom.provider.as_ref().map(|p| &p.key) == provider.as_ref().map(|p| &p.key);
    if same {
        // A new version can change the name and page count.
        custom.provider = provider;
        let pages = custom.provider.as_ref().map_or(1, |p| p.pages);
        custom.page = custom.page.min(pages - 1);
        return Vec::new();
    }
    let mut effects = release_all(state, true);
    let custom = &mut state.custom;
    custom.layout = empty_layout();
    custom.page = 0;
    custom.toggled = Default::default();
    custom.notice = None;
    if let Some(provider) = &provider {
        match layout {
            None => {}
            Some(Ok(bytes)) => match decode(&bytes) {
                Ok((layout, dropped)) => {
                    custom.layout = layout;
                    if dropped > 0 {
                        custom.notice = Some(format!("{dropped} slots were not valid"));
                    }
                }
                Err(error) => {
                    custom.notice = Some(format!("The layout was not read: {error}"));
                    effects.push(Effect::Notice(
                        super::Subsystem::Extensions,
                        format!("{}: {error}", provider.layout_path()),
                    ));
                }
            },
            Some(Err(error)) => effects.push(Effect::Notice(
                super::Subsystem::Extensions,
                format!("{}: {error}", provider.layout_path()),
            )),
        }
    }
    let restore = provider
        .as_ref()
        .is_some_and(|provider| custom.restore.as_ref() == Some(&provider.key));
    custom.provider = provider;
    let footswitch = &mut state.performance.footswitch;
    if footswitch.custom && custom.provider.is_none() {
        // The stored choice stays: the mode comes back with its extension.
        footswitch.custom = false;
        effects.push(Effect::Notice(
            super::Subsystem::Extensions,
            "Custom footswitch mode is off: its extension is off".into(),
        ));
    } else if footswitch.custom {
        // Another installation took over the mode that is on: it is the stored choice now.
        custom.restore = custom.provider.as_ref().map(|p| p.key.clone());
        effects.push(Effect::CustomModeChanged);
    } else if restore {
        effects.extend(enter(state));
    }
    effects
}

/// Turns the mode on (page 1) or off. Off releases the held slots; off while the mode is
/// already off (its extension is off) forgets the stored choice, so a stock mode the user
/// chose stays.
pub fn set_active(state: &mut State, active: bool) -> Vec<Effect> {
    if active == state.performance.footswitch.custom {
        // A stock mode chosen while the mode's extension is off: it does not come back.
        if !active && state.custom.restore.take().is_some() {
            return vec![Effect::CustomModeChanged];
        }
        return Vec::new();
    }
    if active {
        if state.custom.provider.is_none() {
            return vec![Effect::Notice(
                super::Subsystem::Extensions,
                "No extension adds a Custom footswitch mode".into(),
            )];
        }
        let mut effects = enter(state);
        effects.push(Effect::CustomModeChanged);
        effects
    } else {
        let mut effects = release_all(state, true);
        state.performance.footswitch.custom = false;
        state.custom.restore = None;
        effects.push(Effect::CustomModeChanged);
        effects.push(Effect::Present);
        effects
    }
}

fn enter(state: &mut State) -> Vec<Effect> {
    let footswitch = &mut state.performance.footswitch;
    footswitch.custom = true;
    footswitch.overlay = None;
    state.custom.page = 0;
    state.custom.restore = state.custom.provider.as_ref().map(|p| p.key.clone());
    vec![Effect::Present]
}

/// The releases owed by every held slot. `run` false drops them. Waiting presses go.
fn release_all(state: &mut State, run: bool) -> Vec<Effect> {
    state.custom.gestures.clear();
    let held: Vec<Held> = (state.custom.held.iter_mut())
        .filter_map(Option::take)
        .collect();
    match run {
        true => finish(state, held),
        false => Vec::new(),
    }
}

/// The release effects of `held`, in order. Each momentary bypass inverts its block's bypass
/// as the releases before it left it.
fn finish(state: &State, held: Vec<Held>) -> Vec<Effect> {
    let mut pending: Vec<(Site, bool)> = Vec::new();
    let mut effects = Vec::new();
    for held in held {
        effects.extend(held.release);
        let Some(site) = held.invert else {
            continue;
        };
        let now = (pending.iter().find(|(at, _)| *at == site))
            .map(|(_, bypassed)| *bypassed)
            .or_else(|| bypassed(state, site));
        let Some(now) = now else {
            continue;
        };
        pending.retain(|(at, _)| *at != site);
        pending.push((site, !now));
        effects.push(Effect::Action(Action::MomentaryBypass {
            site,
            bypassed: !now,
        }));
    }
    effects
}

/// The bypass of the block at `site` in the playing scene.
fn bypassed(state: &State, site: Site) -> Option<bool> {
    let current = state.current.as_ref()?;
    let block = current.deployment.layout().instance(site)?;
    Some(block.bypass.at(current.deployment.scene()))
}

/// The blocks whose bypass a held momentary slot changed: a save stores them at rest
/// (`preset::save_resting`).
pub fn held_bypass_slots(state: &State) -> Vec<tetrad_engine::Slot> {
    (state.custom.held.iter().flatten())
        .filter_map(|held| match held.invert {
            Some(Site::Slot(slot)) => Some(slot),
            _ => None,
        })
        .collect()
}

/// A preset finished loading: the held momentary bypasses belonged to the old preset's blocks;
/// the other releases (Looper buttons) still run.
pub fn preset_loaded(state: &mut State) {
    for held in state.custom.held.iter_mut().flatten() {
        held.invert = None;
    }
    state.custom.toggled = Default::default();
    state.custom.gestures.clear();
}

pub fn turn(state: &mut State, step: PageStep) -> Vec<Effect> {
    let Some(pages) = (state.custom.provider.as_ref()).map(|p| p.pages) else {
        return Vec::new();
    };
    if !state.performance.footswitch.custom {
        return Vec::new();
    }
    let page = state.custom.page;
    state.custom.gestures.clear();
    state.custom.page = match step {
        PageStep::Next => (page + 1) % pages,
        PageStep::Previous => (page + pages - 1) % pages,
        PageStep::Go(page) if page < pages => page,
        PageStep::Go(_) => page,
    };
    vec![Effect::Present]
}

// ---- presses --------------------------------------------------------------------------------

/// A press or release of slot 0..=9 of the current page. Footswitch presses can wait for a
/// double press or a combo; the others run the press.
pub fn slot(state: &mut State, slot: u8, pressed: bool, origin: Origin) -> Vec<Effect> {
    sync(state);
    if usize::from(slot) >= CUSTOM_SLOTS || !state.performance.footswitch.custom {
        return Vec::new();
    }
    let fired = match (origin, pressed) {
        (Origin::Footswitch, _) => {
            let shapes = shapes(state);
            (state.custom.gestures).edge(slot, pressed, state.now, &shapes)
        }
        (_, true) => vec![Fire::Down {
            slot,
            gesture: Gesture::Press,
        }],
        (_, false) => vec![Fire::Up(slot)],
    };
    fire(state, fired, origin)
}

/// The footswitch presses whose wait ended ([`CustomMode::deadline`]).
pub fn expire(state: &mut State) -> Vec<Effect> {
    sync(state);
    if !state.performance.footswitch.custom {
        state.custom.gestures.clear();
        return Vec::new();
    }
    let shapes = shapes(state);
    let fired = state.custom.gestures.expire(state.now, &shapes);
    fire(state, fired, Origin::Footswitch)
}

/// What the switches of the current page of the playing layout have beyond their press.
fn shapes(state: &State) -> [Shape; CUSTOM_SLOTS] {
    let custom = &state.custom;
    (custom.playing().get(usize::from(custom.page))).map_or_else(Default::default, Page::shapes)
}

/// What the gestures fired, as one [`Action::CustomFire`] each: a press reads the state that the
/// presses before it left (a bypass, the page, the mode).
fn fire(state: &State, fired: Vec<Fire>, origin: Origin) -> Vec<Effect> {
    let page = state.custom.page;
    (fired.into_iter())
        .map(|fire| {
            let (slot, gesture) = match fire {
                Fire::Down { slot, gesture } => (slot, Some(gesture)),
                Fire::Up(slot) => (slot, None),
            };
            Effect::Action(Action::CustomFire {
                slot,
                gesture,
                page,
                origin,
            })
        })
        .collect()
}

/// One fired press or release ([`Action::CustomFire`]). A press fired on another page, or
/// after the mode ended, is dropped.
pub fn fired(
    state: &mut State,
    slot: u8,
    gesture: Option<Gesture>,
    page: u8,
    origin: Origin,
) -> Vec<Effect> {
    sync(state);
    let ran = match gesture {
        Some(_) if !state.performance.footswitch.custom || state.custom.page != page => None,
        Some(gesture) => start(state, slot, gesture, origin),
        None => end(state, slot, origin),
    };
    ran.map_or_else(Vec::new, |mut effects| {
        effects.push(Effect::Present);
        effects
    })
}

/// Starts the `gesture` action of `slot` on the current page; `None` while the switch is held
/// or the action is gone.
fn start(state: &mut State, slot: u8, gesture: Gesture, origin: Origin) -> Option<Vec<Effect>> {
    let index = usize::from(slot);
    if state.custom.held[index].is_some() {
        return None;
    }
    let page = state.custom.page;
    let binding = (state.custom.playing().get(usize::from(page)))
        .and_then(|data| data.binding(slot, gesture))?
        .clone();
    let (effects, release, invert) = match &binding.action {
        None => (Vec::new(), Vec::new(), None),
        Some(action) => run(state, (page, slot, gesture), &binding, action),
    };
    state.custom.held[index] = Some(Held {
        origin,
        page,
        gesture,
        release,
        invert,
    });
    Some(effects)
}

/// Ends what `slot` holds: its own press, or a combo held on its partner.
fn end(state: &mut State, slot: u8, origin: Origin) -> Option<Vec<Effect>> {
    let held = &mut state.custom.held;
    let at = (0..CUSTOM_SLOTS).find(|&index| {
        held[index].as_ref().is_some_and(|held| {
            held.origin == origin
                && (index == usize::from(slot) || held.gesture == Gesture::Combo(slot))
        })
    })?;
    let held = held[at].take()?;
    Some(finish(state, vec![held]))
}

/// The press effects of one action, its release effects and the block a momentary release
/// inverts. `key` is the (page, slot, gesture) of the action.
fn run(
    state: &mut State,
    key: (u8, u8, Gesture),
    data: &Binding,
    action: &StoredAction,
) -> (Vec<Effect>, Vec<Effect>, Option<Site>) {
    let act = |actions: Vec<Action>| actions.into_iter().map(Effect::Action).collect::<Vec<_>>();
    match action {
        StoredAction::System { action } if !system_allowed(*action) => {
            (Vec::new(), Vec::new(), None)
        }
        StoredAction::System {
            action: SystemAction::CustomPage(step),
        } => (turn(state, *step), Vec::new(), None),
        // A Looper button is held while the switch is held.
        StoredAction::System {
            action: SystemAction::Looper(button),
        } => {
            let button = mappings::looper_button(*button);
            let edge = |pressed| vec![Action::Looper { button, pressed }];
            (act(edge(true)), act(edge(false)), None)
        }
        StoredAction::System { action } => {
            let now = state.now;
            let mut tap = state.custom.last_tap;
            let actions = mappings::press(*action, state, now, &mut tap);
            state.custom.last_tap = tap;
            (act(actions), Vec::new(), None)
        }
        StoredAction::Block {
            row,
            column,
            model,
            target,
        } => {
            let Some(position) = mappings::position(*row, *column) else {
                return (Vec::new(), Vec::new(), None);
            };
            let Some(model) = model.or_else(|| mappings::model_at(state, position)) else {
                return (Vec::new(), Vec::new(), None);
            };
            let press = block_press(state, position, model, *target, data.value);
            // A momentary bypass writes as a stock momentary stomp (source 3), and its release
            // inverts the bypass as it then is.
            let (press, invert) = match (press, data.momentary) {
                (Some(Action::SetBypass { site, bypassed }), true) => {
                    (Some(Action::MomentaryBypass { site, bypassed }), Some(site))
                }
                (press, _) => (press, None),
            };
            (act(press.into_iter().collect()), Vec::new(), invert)
        }
        StoredAction::LoadPreset { path, name } => {
            let located = (state.library.as_ref())
                .map(|library| super::preset::locate(&library.presets, path));
            let effect = match located {
                Some(Ok(Some(target))) => Effect::Action(Action::LoadPreset {
                    target,
                    reload: false,
                }),
                Some(Ok(None)) => Effect::Notice(
                    super::Subsystem::Library,
                    format!("{name} is in no setlist slot: assign it again"),
                ),
                Some(Err(error)) => Effect::Notice(super::Subsystem::Library, error.to_string()),
                None => return (Vec::new(), Vec::new(), None),
            };
            (vec![effect], Vec::new(), None)
        }
        StoredAction::SendMidi => {
            let low = state.custom.toggled.contains(&key);
            let mut has_toggle = false;
            let effects = (data.messages.iter())
                .flat_map(|stored| {
                    let message = ui_message(stored);
                    has_toggle |= message.kind == MidiMessageKind::CcToggle;
                    super::midi::out_messages(&message, low)
                })
                .map(Effect::ExtensionMidiOut)
                .collect();
            if has_toggle && !state.custom.toggled.remove(&key) {
                state.custom.toggled.insert(key);
            }
            (effects, Vec::new(), None)
        }
        StoredAction::Command { key, command } => (
            vec![Effect::ExtensionCommand {
                key: key.clone(),
                command: command.clone(),
            }],
            Vec::new(),
            None,
        ),
    }
}

/// A press of a block slot: a bypass toggles, a parameter takes the slot's value (percent).
/// `None` when the block is gone or replaced, or the parameter is text or disabled.
fn block_press(
    state: &State,
    position: tetrad_ui::GridPosition,
    model: u32,
    target: StoredTarget,
    value: Option<u8>,
) -> Option<Action> {
    let current = state.current.as_ref()?;
    let site = Site::Slot(mappings::engine_slot(position)?);
    let instance = current.deployment.layout().instance(site)?;
    if instance.model.0 != model {
        return None;
    }
    Some(match target {
        StoredTarget::Bypass => Action::SetBypass {
            site,
            bypassed: !instance.bypass.at(current.deployment.scene()),
        },
        StoredTarget::Parameter { index } => {
            let spec = state.specs.as_ref()?.model(model)?.parameter(index)?;
            if spec.kind == tetrad_catalog::ParameterType::String || spec.scale.disabled {
                return None;
            }
            Action::SetParameter {
                site,
                index,
                value: f32::from(value.unwrap_or(100).min(100)) / 100.0,
            }
        }
    })
}

// ---- commands -------------------------------------------------------------------------------

/// Resolves a slot of the browsed setlist to its preset file and name
/// ([`SlotAction::PresetSlot`]).
pub type PresetSlots<'a> = &'a dyn Fn(u64) -> Option<(String, String)>;

/// One request of the slot editor or Gig View.
pub fn command(state: &mut State, command: CustomCommand, presets: PresetSlots) -> Step {
    sync(state);
    state.custom.notice = None;
    let step = match command {
        CustomCommand::Tap { slot } => {
            let tap = |pressed| {
                Effect::Action(Action::CustomSlot {
                    slot,
                    pressed,
                    origin: Origin::Touch,
                })
            };
            return Step {
                effects: vec![tap(true), tap(false)],
                save: None,
            };
        }
        CustomCommand::Page(step) => {
            return Step {
                effects: vec![Effect::Action(Action::CustomPage(step))],
                save: None,
            };
        }
        CustomCommand::PresetLayout(own) => preset_layout(state, own),
        CustomCommand::CopyPage { from, .. }
            if state
                .custom
                .provider
                .as_ref()
                .is_none_or(|p| from >= p.pages) =>
        {
            Ok(Step::default())
        }
        CustomCommand::CopyPage { scope, from, to } => {
            let touched = (0..CUSTOM_SLOTS as u8).map(|slot| (to, slot)).collect();
            change(state, scope, touched, |layout| {
                if let Some(page) = layout.get(usize::from(from)).cloned()
                    && let Some(target) = layout.get_mut(usize::from(to))
                {
                    *target = page;
                }
                Ok(())
            })
        }
        CustomCommand::Slot {
            scope,
            page,
            slot,
            gesture,
            edit,
        } => edit_slot(state, scope, (page, slot, gesture), edit, presets),
    };
    step.unwrap_or_else(|reason| {
        state.custom.notice = Some(reason);
        Step::default()
    })
}

/// An edit of a copy of one layout.
type LayoutEdit = Box<dyn FnOnce(&mut Vec<Page>)>;

/// One edit of the `gesture` action of a switch (or a copy or swap of the switch with another
/// of the same layout). An edit other than a new action does nothing to an action that is not
/// there yet.
fn edit_slot(
    state: &mut State,
    scope: LayoutScope,
    (page, slot, gesture): (u8, u8, Gesture),
    edit: SlotEdit,
    presets: PresetSlots,
) -> Result<Step, String> {
    let at = |page: u8, slot: u8| (usize::from(page), usize::from(slot));
    let (p, s) = at(page, slot);
    if s >= CUSTOM_SLOTS {
        return Ok(Step::default());
    }
    let mut touched = vec![(page, slot)];
    if let Gesture::Combo(partner) = gesture {
        if !adjacent(slot, partner) {
            return Err("A combo is two switches side by side or one above the other".into());
        }
        touched.push((page, partner));
    }
    if gesture != Gesture::Press
        && matches!(
            edit,
            SlotEdit::SetColour(_) | SlotEdit::CopyTo { .. } | SlotEdit::MoveTo { .. }
        )
    {
        return Err("The colour, COPY and MOVE belong to the switch: choose PRESS".into());
    }
    // An edit of the binding: the double press or combo is made by a new action only.
    let binding = move |edit: Box<dyn FnOnce(&mut Binding)>, create| -> LayoutEdit {
        Box::new(move |layout| {
            if let Some(binding) = layout[p].binding_mut(s, gesture, create) {
                edit(binding);
            }
        })
    };
    let apply: LayoutEdit = match edit {
        SlotEdit::SetAction(action) => {
            let action = stored_action(state, action, presets)?;
            let set = move |data: &mut Binding| {
                let parameter = matches!(
                    action,
                    StoredAction::Block {
                        target: StoredTarget::Parameter { .. },
                        ..
                    }
                );
                data.value = parameter.then_some(data.value.unwrap_or(100));
                data.momentary = false;
                if !matches!(action, StoredAction::SendMidi) {
                    data.messages.clear();
                }
                data.action = Some(action);
            };
            binding(Box::new(set), true)
        }
        SlotEdit::Clear => Box::new(move |layout| match gesture {
            Gesture::Press => layout[p].slots[s] = Slot::default(),
            Gesture::Double => layout[p].slots[s].double = None,
            Gesture::Combo(partner) => {
                let pair = [slot.min(partner), slot.max(partner)];
                layout[p].combos.retain(|combo| combo.switches != pair);
            }
        }),
        SlotEdit::SetLabel(label) => {
            let label = label.trim().to_owned();
            if label.chars().count() > MAX_LABEL || label.chars().any(char::is_control) {
                return Err(format!("A label has at most {MAX_LABEL} characters"));
            }
            let set = move |data: &mut Binding| data.label = (!label.is_empty()).then_some(label);
            binding(Box::new(set), false)
        }
        SlotEdit::SetColour(swatch) => {
            if swatch.is_some_and(|swatch| usize::from(swatch) >= PALETTE.len()) {
                return Err("There are eight colours".into());
            }
            Box::new(move |layout| layout[p].slots[s].colour = swatch)
        }
        SlotEdit::SetValue(value) => binding(
            Box::new(move |data| {
                if data.value.is_some() {
                    data.value = Some(value.min(100));
                }
            }),
            false,
        ),
        SlotEdit::SetMomentary(momentary) => {
            binding(Box::new(move |data| data.momentary = momentary), false)
        }
        SlotEdit::SetMessages(messages) => {
            if messages.len() > MAX_MESSAGES {
                return Err(format!("A slot sends at most {MAX_MESSAGES} messages"));
            }
            let messages: Vec<_> = messages.iter().map(stored_message).collect();
            binding(Box::new(move |data| data.messages = messages), false)
        }
        SlotEdit::CopyTo { slot, .. } | SlotEdit::MoveTo { slot, .. }
            if usize::from(slot) >= CUSTOM_SLOTS =>
        {
            return Err(format!("There is no switch {}", u16::from(slot) + 1));
        }
        SlotEdit::CopyTo { page, slot } => {
            touched.push((page, slot));
            let (tp, ts) = at(page, slot);
            Box::new(move |layout| {
                if tp < layout.len() {
                    layout[tp].slots[ts] = layout[p].slots[s].clone();
                }
            })
        }
        SlotEdit::MoveTo { page, slot } => {
            touched.push((page, slot));
            let (tp, ts) = at(page, slot);
            Box::new(move |layout| {
                if tp < layout.len() && (tp, ts) != (p, s) {
                    let moved = std::mem::take(&mut layout[p].slots[s]);
                    layout[p].slots[s] = std::mem::replace(&mut layout[tp].slots[ts], moved);
                }
            })
        }
    };
    change(state, scope, touched, |layout| {
        apply(layout);
        Ok(())
    })
}

/// Runs `edit` on a copy of the layout of `scope` and stores it when it changed: the file for
/// ALL PRESETS, a preset edit for THIS PRESET. Every `touched` (page, slot) out of the mode's
/// pages refuses the edit; a held press of a touched slot of the playing layout ends.
fn change(
    state: &mut State,
    scope: LayoutScope,
    touched: Vec<(u8, u8)>,
    edit: impl FnOnce(&mut Vec<Page>) -> Result<(), String>,
) -> Result<Step, String> {
    let Some(pages) = state.custom.provider.as_ref().map(|p| p.pages) else {
        return Ok(Step::default());
    };
    if touched.iter().any(|(page, _)| *page >= pages) {
        return Ok(Step::default());
    }
    let custom = &state.custom;
    let mut layout = match scope {
        LayoutScope::Global => custom.layout.clone(),
        LayoutScope::Preset => (custom.preset_layout.clone())
            .ok_or_else(|| "This preset uses the layout of all presets".to_owned())?,
    };
    let before = layout.clone();
    edit(&mut layout)?;
    if layout == before {
        return Ok(Step::default());
    }
    // Only the playing layout has held and waiting presses and CC Toggle states to end.
    let mut effects = Vec::new();
    if scope == state.custom.playing_scope() {
        state.custom.gestures.clear();
        let mut ended = Vec::new();
        for (page, slot) in &touched {
            let index = usize::from(*slot);
            ended.extend(state.custom.held[index].take_if(|held| held.page == *page));
            (state.custom.toggled).retain(|(p, s, _)| (p, s) != (page, slot));
        }
        // One pass, so two held bypasses of one block end where they started.
        effects.extend(finish(state, ended));
    }
    effects.push(Effect::Present);
    let bytes = encode(&layout);
    let save = match scope {
        LayoutScope::Global => {
            state.custom.layout = layout;
            (state.custom.provider.as_ref()).map(|p| (p.layout_path(), bytes))
        }
        LayoutScope::Preset => {
            state.custom.preset_layout = Some(layout);
            let edit = PresetEdit::CustomLayout(Some(bytes));
            effects.push(Effect::Action(Action::PresetEdit(edit)));
            None
        }
    };
    Ok(Step { effects, save })
}

/// THIS PRESET gets a copy of the ALL PRESETS layout, or loses its own.
fn preset_layout(state: &mut State, own: bool) -> Result<Step, String> {
    if state.current.is_none() {
        return Err("No preset is loaded".into());
    }
    let custom = &mut state.custom;
    if own == custom.preset_layout.is_some() {
        return Ok(Step::default());
    }
    // The held presses keep their releases; a held slot of the old layout ends normally. A
    // waiting press belongs to the old layout.
    custom.gestures.clear();
    custom.preset_layout = own.then(|| custom.layout.clone());
    let bytes = custom.preset_layout.as_deref().map(encode);
    Ok(Step {
        effects: vec![
            Effect::Action(Action::PresetEdit(PresetEdit::CustomLayout(bytes))),
            Effect::Present,
        ],
        save: None,
    })
}

/// A chosen action as stored, or why the slot cannot take it.
fn stored_action(
    state: &State,
    action: SlotAction,
    presets: PresetSlots,
) -> Result<StoredAction, String> {
    Ok(match action {
        SlotAction::System(action) if !system_allowed(action) => {
            return Err("A slot cannot press another slot".into());
        }
        SlotAction::System(action) => StoredAction::System { action },
        SlotAction::Block { position, target } => {
            let model = mappings::model_at(state, position)
                .ok_or_else(|| "There is no block there".to_owned())?;
            StoredAction::Block {
                row: position.row(),
                column: position.column(),
                model: matches!(target, BlockTarget::Parameter(_)).then_some(model),
                target: match target {
                    BlockTarget::Bypass => StoredTarget::Bypass,
                    BlockTarget::Parameter(index) => StoredTarget::Parameter { index },
                },
            }
        }
        SlotAction::LoadPreset => {
            let current =
                (state.current.as_ref()).ok_or_else(|| "No preset is loaded".to_owned())?;
            let path = (current.path.clone())
                .ok_or_else(|| "Save the preset first: it has no file".to_owned())?;
            StoredAction::LoadPreset {
                path,
                name: (current.preset.name.clone()).unwrap_or_else(|| "Preset".into()),
            }
        }
        SlotAction::PresetSlot(slot) => {
            let (path, name) = presets(slot).ok_or_else(|| "That slot is empty".to_owned())?;
            StoredAction::LoadPreset { path, name }
        }
        SlotAction::SendMidi => StoredAction::SendMidi,
        SlotAction::Command { key, command } => {
            let known = (state.custom.commands.iter())
                .any(|choice| choice.key == key && choice.command == command);
            if !known {
                return Err("That extension command is not running".into());
            }
            StoredAction::Command { key, command }
        }
    })
}

fn stored_message(message: &MidiOutMessage) -> StoredMessage {
    StoredMessage {
        kind: match message.kind {
            MidiMessageKind::Cc => StoredKind::Cc,
            MidiMessageKind::CcToggle => StoredKind::CcToggle,
            MidiMessageKind::Pc => StoredKind::Pc,
        },
        channel: message.channel.clamp(1, 16),
        values: message
            .values
            .map(|value| value.map(|value| value.min(127))),
    }
}

fn ui_message(message: &StoredMessage) -> MidiOutMessage {
    MidiOutMessage {
        kind: match message.kind {
            StoredKind::Cc => MidiMessageKind::Cc,
            StoredKind::CcToggle => MidiMessageKind::CcToggle,
            StoredKind::Pc => MidiMessageKind::Pc,
        },
        channel: message.channel,
        values: message.values,
    }
}

// ---- views ----------------------------------------------------------------------------------

/// The slot editor and Gig View contents; `None` without a provider.
pub fn view(state: &State) -> Option<CustomModeView> {
    let custom = &state.custom;
    let provider = custom.provider.as_ref()?;
    let playing = custom.playing_scope();
    let views = |layout: &[Page], scope| {
        (0..provider.pages)
            .map(|page| match layout.get(usize::from(page)) {
                Some(data) => page_view(state, data, page, scope == playing),
                None => CustomPageView::default(),
            })
            .collect::<Vec<_>>()
    };
    let slots = views(&custom.layout, LayoutScope::Global);
    let preset_slots =
        (custom.preset_layout.as_deref()).map(|layout| views(layout, LayoutScope::Preset));
    Some(CustomModeView {
        name: provider.name.clone(),
        extension: provider.extension.clone(),
        pages: provider.pages,
        page: custom.page,
        active: state.performance.footswitch.custom,
        slots,
        preset_slots,
        preset: (state.current.as_ref())
            .and_then(|current| current.preset.name.clone())
            .unwrap_or_default(),
        commands: custom.commands.clone(),
        colours: PALETTE,
        notice: custom.notice.clone(),
    })
}

/// The LEDs of the current page: the press of each switch.
pub fn leds(state: &State) -> [Option<CustomLed>; CUSTOM_SLOTS] {
    let page = state.custom.page;
    std::array::from_fn(|index| {
        let data = state.custom.slot(page, index)?;
        data.press.action.as_ref()?;
        let at = (page, index as u8, Gesture::Press);
        let (_, _, colour, engaged, _) = describe(state, at, &data.press, data.colour, true);
        Some(CustomLed {
            rgb: colour,
            engaged,
        })
    })
}

/// One page of a layout; `playing` is true for the layout the switches play.
fn page_view(state: &State, data: &Page, page: u8, playing: bool) -> CustomPageView {
    let slots = std::array::from_fn(|index| {
        let slot = &data.slots[index];
        let at = |gesture| (page, index as u8, gesture);
        let double = (slot.double.as_ref()).map(|double| {
            Box::new(binding_view(
                state,
                at(Gesture::Double),
                double,
                None,
                playing,
            ))
        });
        CustomSlotView {
            double,
            ..binding_view(state, at(Gesture::Press), &slot.press, slot.colour, playing)
        }
    });
    let combos = (data.combos.iter())
        .map(|combo| {
            let [first, second] = combo.switches;
            let at = (page, first, Gesture::Combo(second));
            CustomComboView {
                switches: combo.switches,
                binding: binding_view(state, at, &combo.binding, None, playing),
            }
        })
        .collect();
    CustomPageView { slots, combos }
}

/// One action of a switch, with the switch's colour choice for its press.
fn binding_view(
    state: &State,
    at: (u8, u8, Gesture),
    data: &Binding,
    swatch: Option<u8>,
    playing: bool,
) -> CustomSlotView {
    let Some(stored) = &data.action else {
        return CustomSlotView {
            label: String::new(),
            detail: format!("{} · empty", SLOT_NAMES[usize::from(at.1)]),
            colour: [0; 3],
            available: true,
            ..CustomSlotView::default()
        };
    };
    let (label, detail, colour, engaged, available) = describe(state, at, data, swatch, playing);
    CustomSlotView {
        action: ui_action(stored),
        label: data.label.clone().unwrap_or(label),
        custom_label: data.label.clone(),
        detail,
        colour,
        swatch,
        engaged,
        available,
        value: data.value,
        momentary: matches!(
            stored,
            StoredAction::Block {
                target: StoredTarget::Bypass,
                ..
            }
        )
        .then_some(data.momentary),
        messages: data.messages.iter().map(ui_message).collect(),
        double: None,
    }
}

fn ui_action(stored: &StoredAction) -> Option<SlotAction> {
    Some(match stored {
        StoredAction::System { action } => SlotAction::System(*action),
        StoredAction::Block {
            row,
            column,
            target,
            ..
        } => SlotAction::Block {
            position: mappings::position(*row, *column)?,
            target: mappings::ui_target(*target),
        },
        StoredAction::LoadPreset { .. } => SlotAction::LoadPreset,
        StoredAction::SendMidi => SlotAction::SendMidi,
        StoredAction::Command { key, command } => SlotAction::Command {
            key: key.clone(),
            command: command.clone(),
        },
    })
}

/// Label, detail, colour, engaged and available of an assigned action at (page, slot,
/// gesture). Only an action of the playing layout shows its held press.
fn describe(
    state: &State,
    (page, slot, gesture): (u8, u8, Gesture),
    data: &Binding,
    swatch: Option<u8>,
    playing: bool,
) -> (String, String, [u8; 3], bool, bool) {
    let held = playing
        && state.custom.held[usize::from(slot)]
            .as_ref()
            .is_some_and(|held| held.page == page && held.gesture == gesture);
    let swatch = swatch.map(|colour| PALETTE[usize::from(colour) % PALETTE.len()]);
    let current = state.current.as_ref();
    let Some(action) = &data.action else {
        return (String::new(), String::new(), [0; 3], false, true);
    };
    let (label, detail, colour, engaged, available) = match action {
        StoredAction::System { action } => {
            let (label, detail) = system_label(*action);
            let engaged = match *action {
                SystemAction::Scene(scene) => current.is_some_and(|current| {
                    current.deployment.scene().index() == usize::from(scene)
                }),
                SystemAction::Tuner => state.screen == Screen::Tuner,
                SystemAction::GigView => state.screen == Screen::Gig,
                _ => held,
            };
            let colour = match *action {
                SystemAction::Scene(scene) => scene_colour(state, scene),
                SystemAction::CustomPage(_) => WHITE,
                SystemAction::Looper(_) => PALETTE[7],
                _ => PALETTE[0],
            };
            (label, detail, colour, engaged, true)
        }
        StoredAction::Block {
            row,
            column,
            model,
            target,
        } => {
            let position = mappings::position(*row, *column);
            let at = position.and_then(|position| mappings::model_at(state, position));
            let available = at.is_some() && model.is_none_or(|model| Some(model) == at);
            let site = position.and_then(mappings::engine_slot).map(Site::Slot);
            let instance = current.zip(site).and_then(|(current, site)| {
                (current.deployment.layout().instance(site)).map(|block| (current, block))
            });
            let engaged = available
                && match target {
                    StoredTarget::Bypass => instance.is_some_and(|(current, block)| {
                        !block.bypass.at(current.deployment.scene())
                    }),
                    StoredTarget::Parameter { .. } => held,
                };
            let colour = (instance.filter(|_| available)).map_or(PALETTE[4], |(_, block)| {
                block_colour(&LedBlock {
                    category: (state.specs.as_ref())
                        .and_then(|specs| specs.model(block.model.0))
                        .map_or(u32::MAX, |spec| spec.category_id),
                    sub_type: 0,
                    model_id: block.model.0,
                })
            });
            let (label, detail) = match position.filter(|_| available) {
                Some(position) => {
                    let name = mappings::block_name(state, position).unwrap_or_default();
                    let target_name =
                        mappings::target_name(state, position, mappings::ui_target(*target));
                    let detail = match target {
                        StoredTarget::Bypass if data.momentary => "Bypass · momentary".into(),
                        StoredTarget::Bypass => "Bypass".into(),
                        StoredTarget::Parameter { .. } => {
                            format!("{target_name} {} %", data.value.unwrap_or(100))
                        }
                    };
                    (name, detail)
                }
                None => (
                    "Missing block".into(),
                    "No such block in this preset".into(),
                ),
            };
            (label, detail, colour, engaged, available)
        }
        StoredAction::LoadPreset { path, name } => {
            let loaded = current.is_some_and(|current| current.path.as_ref() == Some(path));
            (name.clone(), "Load preset".into(), PALETTE[6], loaded, true)
        }
        StoredAction::SendMidi => {
            let detail = match data.messages.as_slice() {
                [] => "No messages yet".into(),
                [one] => mappings::message_text(&ui_message(one)),
                [first, rest @ ..] => format!(
                    "{} +{}",
                    mappings::message_text(&ui_message(first)),
                    rest.len()
                ),
            };
            ("MIDI".into(), detail, PALETTE[5], held, true)
        }
        StoredAction::Command { key, command } => {
            let choice = (state.custom.commands.iter())
                .find(|choice| &choice.key == key && &choice.command == command);
            match choice {
                Some(choice) => {
                    let (extension, label) = choice
                        .label
                        .split_once(" · ")
                        .unwrap_or(("Extension", &choice.label));
                    (
                        label.to_owned(),
                        extension.to_owned(),
                        PALETTE[3],
                        held,
                        true,
                    )
                }
                None => (
                    command.clone(),
                    "The extension is not running".into(),
                    PALETTE[3],
                    false,
                    false,
                ),
            }
        }
    };
    (label, detail, swatch.unwrap_or(colour), engaged, available)
}

fn scene_colour(state: &State, scene: u8) -> [u8; 3] {
    (state.current.as_ref())
        .and_then(|current| current.preset.scene_colors.get(usize::from(scene)))
        .map_or(PALETTE[usize::from(scene) % 8], |&raw| {
            let [_, red, green, blue] = raw.to_be_bytes();
            [red, green, blue]
        })
}

/// The tile label and detail of a system action.
fn system_label(action: SystemAction) -> (String, String) {
    let (label, detail) = mappings::system_text(action);
    match action {
        SystemAction::Tuner | SystemAction::GigView => (label, detail),
        SystemAction::CustomPage(PageStep::Next) => ("Page ›".into(), "Next page".into()),
        SystemAction::CustomPage(PageStep::Previous) => ("‹ Page".into(), "Previous page".into()),
        SystemAction::CustomPage(PageStep::Go(page)) => {
            (format!("Page {}", page + 1), "Go to page".into())
        }
        _ => (detail, label),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stored layout keeps every field of an assigned slot, its double press and the
    /// combos, reads a file from before double presses as before, and drops entries it cannot
    /// use.
    #[test]
    fn layouts_round_trip_and_drop_bad_entries() {
        let mut layout = empty_layout();
        let tuner = Binding {
            action: Some(StoredAction::System {
                action: SystemAction::Tuner,
            }),
            ..Binding::default()
        };
        layout[1].slots[9] = Slot {
            press: Binding {
                action: Some(StoredAction::Block {
                    row: 0,
                    column: 2,
                    model: Some(4004),
                    target: StoredTarget::Parameter { index: 3 },
                }),
                label: Some("Lead boost".into()),
                value: Some(80),
                momentary: false,
                messages: Vec::new(),
            },
            colour: Some(2),
            double: Some(tuner.clone()),
        };
        layout[3].slots[0].press = Binding {
            action: Some(StoredAction::SendMidi),
            messages: vec![StoredMessage {
                kind: StoredKind::CcToggle,
                channel: 3,
                values: [Some(20), Some(0), Some(127)],
            }],
            ..Binding::default()
        };
        layout[0].combos.push(Combo {
            switches: [2, 3],
            binding: Binding {
                label: Some("Lead".into()),
                ..tuner
            },
        });
        let bytes = encode(&layout);
        assert_eq!(decode(&bytes).unwrap(), (layout, 0));

        let bad = br#"{"format":1,"slots":[
            {"page":4,"slot":0,"action":{"kind":"sendMidi"}},
            {"page":0,"slot":10,"action":{"kind":"sendMidi"}},
            {"page":0,"slot":1,"action":{"kind":"system","action":{"customSlot":3}}},
            {"page":0,"slot":2,"colour":9},
            {"page":0,"slot":4,"double":{"action":{"kind":"system","action":{"customSlot":1}}}},
            {"page":0,"slot":3,"action":{"kind":"system","action":"tuner"},"label":"Tune"}
        ],"combos":[
            {"page":0,"switches":[0,5],"action":{"kind":"sendMidi"}},
            {"page":0,"switches":[3,2]},
            {"page":0,"switches":[9,8],"action":{"kind":"sendMidi"}},
            {"page":0,"switches":[8,9],"action":{"kind":"sendMidi"}}
        ]}"#;
        let (layout, dropped) = decode(bad).unwrap();
        assert_eq!(dropped, 8);
        assert_eq!(layout[0].slots[3].press.label.as_deref(), Some("Tune"));
        assert_eq!(layout[0].combos.len(), 1);
        assert_eq!(layout[0].combos[0].switches, [8, 9]);
        assert!(decode(br#"{"format":2,"slots":[]}"#).is_err());
    }
}
