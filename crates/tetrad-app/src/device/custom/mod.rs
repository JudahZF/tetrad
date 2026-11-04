//! Custom footswitch mode (owned, Tetrad; `tetrad_ui::custom_mode`): Tetrad's footswitch mode in
//! the place of stock Stomp mode. It is on while the stored mode (`Mode/mode`) is Stomp, so
//! stock CorOS, Cortex Control and MIDI CC#47 see Stomp mode. A stored Stomp hybrid (Tetrad
//! offers none) also plays as Custom. While it is on, the ten switches A–D, Down, E–H and Tap
//! are the slots of the current page and Bank Up is the mode switch (tetrad-core
//! `FootswitchContext::press`).
//!
//! A press captures what its release must do, so a release after a page change still ends the
//! action it started (a momentary bypass comes back). Leaving the mode and a new layout of a
//! held slot release every held slot; a preset load drops the held momentary bypasses and
//! parameters, which belong to the old preset's blocks.
//!
//! A switch can also have a double press, and two adjacent switches a combo. Footswitch presses
//! (and MIDI CC#35..=42, which are footswitch taps: double presses, never combos) go through
//! `gesture::Gestures`, which decides which of them runs; Gig View taps and Mappings presses run
//! the press at once. A page change, leaving the mode, a preset load and a layout edit drop a
//! press that still waits.
//!
//! The ALL PRESETS layout is in [`LAYOUT_FILE`]. A preset can have its own layout (THIS PRESET)
//! in the preset field [`TETRAD_FOOTSWITCH_FIELD`]; while the preset is loaded, each of its
//! switches with actions plays instead of the ALL PRESETS switch, and its edits are preset
//! edits that the preset saves. [`ACTIVE_FILE`] keeps the double press and combo times.

mod gesture;
mod stock;

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tetrad_core::performance::leds::{CustomLed, LedBlock, PALETTE, block_colour};
use tetrad_engine::Site;
use tetrad_preset::model::TETRAD_FOOTSWITCH_FIELD;
use tetrad_ui::{
    ActionEdit, BlockTarget, COMBO_PRESS, COMBO_PRESS_CHOICES, CUSTOM_SLOTS, CommandChoice,
    CustomActionView, CustomComboView, CustomCommand, CustomModeView, CustomPageView,
    CustomSlotView, DOUBLE_PRESS, DOUBLE_PRESS_CHOICES, Gesture, LayoutScope, MAX_ACTIONS,
    MidiMessageKind, MidiOutMessage, ModeKind, PageStep, SLOT_NAMES, Screen, SettingId,
    SettingValue, SlotAction, SlotEdit, SystemAction, adjacent,
};

use self::gesture::{Fire, Gestures, Shape};

use super::actions::ActionError;
use super::mappings::mode::{self, Mode, Param, Source, Target, Window};
use super::mappings::{self, StoredTarget, SystemDef};
use super::{Action, Effect, State};

/// The installation whose mode was on at the last change, and the double press and combo
/// times.
pub const ACTIVE_FILE: &str = "/media/p4/tetrad/footswitch-mode.json";
/// One layout per installation: `<install key file stem>.json`.
pub const LAYOUT_DIR: &str = "/media/p4/tetrad/footswitch-modes";
/// Largest layout or active file. Read them with `read_bounded(.., MAX_FILE)`. A full layout
/// (four pages of presses, double presses and all 13 combos, each with eight actions of 12
/// messages and a 16-character label) is about 2.7 MB.
pub const MAX_FILE: u64 = 4 * 1024 * 1024;
/// Format 2 gives each press, double press and combo a list of actions with Mappings modes;
/// format 1 (one action, a percent value, a momentary flag) is read and written as format 2.
const LAYOUT_FORMAT: u32 = 2;
const ACTIVE_FORMAT: u32 = 1;
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

/// The mode's name, as the mode button shows it.
pub const NAME: &str = "Custom";
/// The ALL PRESETS layout.
pub const LAYOUT_FILE: &str = "/media/p4/tetrad/footswitch-layout.json";
/// Where Tetrad 0.1.0-alpha.10 and earlier kept the layout of the extension that added the
/// mode (`<install key file stem>.json`): read once while [`LAYOUT_FILE`] is missing.
pub const OLD_LAYOUT_DIR: &str = "/media/p4/tetrad/footswitch-modes";

/// The stored footswitch mode plays as Custom: stock Stomp mode, or a Stomp hybrid.
pub fn plays_custom(mode: tetrad_settings::controls::FootswitchMode) -> bool {
    use tetrad_core::performance::mode::FootswitchKind;
    super::actions::core_mode(mode).is_some_and(|mode| mode.includes(FootswitchKind::Stomp))
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
    /// THIS PRESET only: the switch plays nothing, not the ALL PRESETS switch.
    Nothing,
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

/// One action of a binding, with its Mappings mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Act {
    action: StoredAction,
    /// `None`: the first mode the action allows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<Mode>,
    /// Send MIDI only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    messages: Vec<StoredMessage>,
    /// The preset's stock Stomp data ([`stock`]): never in a layout field, and a press plays
    /// it through the stock stomp path.
    #[serde(skip)]
    stock: bool,
}

impl Act {
    fn new(action: StoredAction) -> Act {
        Act {
            action,
            mode: None,
            messages: Vec::new(),
            stock: false,
        }
    }

    /// The stored mode, or the first one the action allows.
    fn mode(&self) -> Mode {
        self.mode.unwrap_or_else(|| {
            let kind = (mode::modes(Source::Press, target(&self.action)).first())
                .copied()
                .unwrap_or(ModeKind::OnPress);
            Mode::fresh(kind, 1.0, 0.0)
        })
    }

    fn valid(&self) -> bool {
        let action = match &self.action {
            StoredAction::System { action } => system_allowed(*action),
            StoredAction::Block { row, column, .. } => mappings::position(*row, *column).is_some(),
            _ => true,
        };
        let modes = mode::modes(Source::Press, target(&self.action));
        let mode = (self.mode).is_none_or(|mode| modes.contains(&mode.kind()) && mode.valid());
        action
            && mode
            && self.messages.len() <= MAX_MESSAGES
            && self.messages.iter().all(|message| {
                (1..=16).contains(&message.channel)
                    && message.values.iter().flatten().all(|v| *v < 128)
            })
    }
}

/// The actions of the press of a switch, its double press or a combo, and its label.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Binding {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    actions: Vec<Act>,
    /// Page 1's A–H of the loaded preset: the preset has stock Stomp data for the switch
    /// ([`stock`]), which a press plays even when no action shows it (a secondary assignment).
    #[serde(skip)]
    stock: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    /// Format 1, read only ([`Binding::upgrade`]): one action, a parameter's percent and a
    /// bypass's momentary flag.
    #[serde(default, skip_serializing)]
    action: Option<StoredAction>,
    #[serde(default, skip_serializing)]
    value: Option<u8>,
    #[serde(default, skip_serializing)]
    momentary: bool,
    #[serde(default, skip_serializing)]
    messages: Vec<StoredMessage>,
}

impl Binding {
    /// A format 1 action becomes the first action, with the mode that does what it did: a
    /// parameter's percent is Set, a momentary bypass and a Looper button (held while the
    /// switch is held) are Momentary.
    fn upgrade(&mut self) {
        let Some(action) = self.action.take() else {
            return;
        };
        let held = Mode::fresh(ModeKind::Momentary, 1.0, 0.0);
        let mode = match &action {
            StoredAction::Block {
                target: StoredTarget::Parameter { .. },
                ..
            } => Some(Mode::Set {
                value: f32::from(self.value.unwrap_or(100).min(100)) / 100.0,
            }),
            StoredAction::Block { .. } if self.momentary => Some(held),
            StoredAction::System {
                action: SystemAction::Looper(_),
            } => Some(held),
            _ => None,
        };
        let messages = std::mem::take(&mut self.messages);
        (self.value, self.momentary) = (None, false);
        if self.actions.is_empty() {
            self.actions.push(Act {
                mode,
                messages,
                ..Act::new(action)
            });
        }
    }

    fn valid(&self) -> bool {
        let label = (self.label.as_ref())
            .is_none_or(|label| (1..=MAX_LABEL).contains(&label.chars().count()));
        label && self.actions.len() <= MAX_ACTIONS && self.actions.iter().all(Act::valid)
    }
}

/// One switch: its press (stored flat, as before double presses), its colour and its double
/// press.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    fn upgrade(&mut self) {
        self.press.upgrade();
        if let Some(double) = &mut self.double {
            double.upgrade();
        }
    }

    fn valid(&self) -> bool {
        self.press.valid()
            && self
                .colour
                .is_none_or(|colour| usize::from(colour) < PALETTE.len())
            && self.double.as_ref().is_none_or(Binding::valid)
    }
}

/// Two adjacent switches pressed together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Combo {
    /// Lower first.
    switches: [u8; 2],
    #[serde(flatten)]
    binding: Binding,
}

#[derive(Debug, Clone, Default, PartialEq)]
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

    /// A double press or combo without actions goes.
    fn prune(&mut self) {
        for slot in &mut self.slots {
            slot.double.take_if(|double| double.actions.is_empty());
        }
        self.combos
            .retain(|combo| !combo.binding.actions.is_empty());
    }

    /// What each switch has beyond its press, for [`Gestures`].
    fn shapes(&self) -> [Shape; CUSTOM_SLOTS] {
        let mut shapes = std::array::from_fn(|index| Shape {
            double: (self.slots[index].double.as_ref()).is_some_and(|d| !d.actions.is_empty()),
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

/// [`LAYOUT_FILE`]: the assigned slots and the combos. A Tetrad without combos and
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
    /// Tetrad 0.1.0-alpha.10 and earlier: the install key of the extension whose mode was on.
    /// Read once to find its layout in [`OLD_LAYOUT_DIR`]; not written.
    #[serde(default, skip_serializing)]
    active: Option<String>,
    /// Settings › Double Press & Combo, in ms; without them, the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    double_press_ms: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    combo_press_ms: Option<u16>,
}

/// A stored time when it is one of `choices`, else `default`.
fn stored_time(ms: Option<u16>, choices: &[u16], default: Duration) -> Duration {
    ms.filter(|ms| choices.contains(ms))
        .map_or(default, |ms| Duration::from_millis(ms.into()))
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
    /// What the release sets in the loaded preset's blocks: momentary parameters' values from
    /// before the press. A preset load drops it.
    restore: Vec<Effect>,
    /// Momentary bypasses: the release inverts these blocks' bypass as it is then, as a stock
    /// momentary release does, so overlapping presses of one block end where they started.
    invert: Vec<Site>,
    /// The actions of the press, as they were at the press; each runs as [`act`].
    actions: Vec<Act>,
    /// The stock stomp (A–H on page 1) whose blocks the press toggled through the stock path.
    stomp: Option<u8>,
}

/// What a command asks the device to do.
#[derive(Debug, Default)]
pub struct Step {
    pub effects: Vec<Effect>,
    /// New contents of the layout file.
    pub save: Option<(String, Vec<u8>)>,
}

#[derive(Debug, Default)]
pub struct CustomMode {
    /// ALL PRESETS: always [`PAGES`] pages.
    layout: Vec<Page>,
    /// THIS PRESET: the loaded preset's own layout, decoded at `preset_revision` ([`sync`]).
    preset_layout: Option<Vec<Page>>,
    preset_revision: Option<u64>,
    page: u8,
    held: [Option<Held>; CUSTOM_SLOTS],
    /// Footswitch presses that wait for a second press or a combo partner.
    gestures: Gestures,
    /// (page, slot, gesture, action) whose CC Toggle messages send the low value next. A
    /// combo is keyed on its lower switch.
    toggled: HashSet<(u8, u8, Gesture, u8)>,
    last_tap: Option<Duration>,
    /// Commands of the running extensions, for the Command action.
    commands: Vec<CommandChoice>,
    notice: Option<String>,
    /// The old [`ACTIVE_FILE`]'s install key, until the layout is read.
    old_active: Option<String>,
}

impl CustomMode {
    pub fn page(&self) -> u8 {
        self.page
    }

    /// [`ACTIVE_FILE`] at start-up. A bad file is reported and ignored.
    pub fn load_active(&mut self, bytes: Option<&[u8]>) -> Option<String> {
        let bytes = bytes?;
        match serde_json::from_slice::<StoredActive>(bytes) {
            Ok(stored) if stored.format == ACTIVE_FORMAT => {
                self.old_active = stored.active;
                self.gestures.set_times(
                    stored_time(stored.double_press_ms, &DOUBLE_PRESS_CHOICES, DOUBLE_PRESS),
                    stored_time(stored.combo_press_ms, &COMBO_PRESS_CHOICES, COMBO_PRESS),
                );
                None
            }
            Ok(stored) => Some(format!(
                "{ACTIVE_FILE}: format {} is unknown",
                stored.format
            )),
            Err(error) => Some(format!("{ACTIVE_FILE}: {error}")),
        }
    }

    /// The contents of [`ACTIVE_FILE`]: the double press and combo times.
    pub fn active_file(&self) -> Vec<u8> {
        let (double, combo) = self.gestures.times();
        let ms = |time: Duration| u16::try_from(time.as_millis()).ok();
        let stored = StoredActive {
            format: ACTIVE_FORMAT,
            active: None,
            double_press_ms: ms(double),
            combo_press_ms: ms(combo),
        };
        serde_json::to_vec_pretty(&stored).expect("the active file encodes")
    }

    /// The file name of the old layout to read when [`LAYOUT_FILE`] is missing: the one of the
    /// extension whose mode was on, if the old [`ACTIVE_FILE`] names one.
    pub fn old_layout_stem(&self) -> Option<String> {
        let key = self.old_active.as_deref()?;
        let key: tetrad_extensions::store::InstallKey = key.parse().ok()?;
        Some(key.file_stem())
    }

    /// The ALL PRESETS layout as stored ([`LAYOUT_FILE`]).
    pub fn layout_file(&self) -> Vec<u8> {
        encode(&self.layout)
    }

    pub fn set_commands(&mut self, commands: Vec<CommandChoice>) {
        self.commands = commands;
    }

    /// The page the switches play: each press, double press and combo of the preset's own
    /// layout that has actions, else the ALL PRESETS one. The colour goes with the press.
    fn playing_page(&self, page: u8) -> Page {
        let index = usize::from(page);
        let mut playing = self.layout.get(index).cloned().unwrap_or_default();
        let Some(own) = (self.preset_layout.as_ref()).and_then(|layout| layout.get(index)) else {
            return playing;
        };
        for (slot, mine) in playing.slots.iter_mut().zip(&own.slots) {
            if !mine.press.actions.is_empty() || mine.press.stock {
                (slot.press, slot.colour) = (mine.press.clone(), mine.colour);
            }
            if (mine.double.as_ref()).is_some_and(|double| !double.actions.is_empty()) {
                slot.double = mine.double.clone();
            }
        }
        for combo in &own.combos {
            playing
                .combos
                .retain(|other| other.switches != combo.switches);
            playing.combos.push(combo.clone());
        }
        playing
    }

    /// When a waiting footswitch press is due: call [`expire`] then.
    pub fn deadline(&self) -> Option<Duration> {
        self.gestures.deadline()
    }

    /// Drops the presses that wait: an extension program has the footswitches now.
    pub fn drop_waiting(&mut self) {
        self.gestures.clear();
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
        format: LAYOUT_FORMAT,
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
    if !(1..=LAYOUT_FORMAT).contains(&stored.format) {
        return Err(format!("format {} is unknown", stored.format));
    }
    let mut layout = empty_layout();
    let mut dropped = 0;
    for mut entry in stored.slots {
        entry.slot_data.upgrade();
        let target = (layout.get_mut(usize::from(entry.page)))
            .and_then(|page| page.slots.get_mut(usize::from(entry.slot)));
        match target {
            Some(slot) if entry.slot_data.valid() => *slot = entry.slot_data,
            _ => dropped += 1,
        }
    }
    for StoredCombo { page, mut combo } in stored.combos {
        combo.binding.upgrade();
        combo.switches.sort_unstable();
        let [a, b] = combo.switches;
        let valid = adjacent(a, b) && !combo.binding.actions.is_empty() && combo.binding.valid();
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
    if state.custom.preset_revision == Some(revision) {
        return;
    }
    state.custom.preset_revision = Some(revision);
    let own = (state.current.as_ref())
        .and_then(|current| current.preset.unknown.bytes_field(TETRAD_FOOTSWITCH_FIELD))
        .and_then(|bytes| decode(&bytes).ok())
        .map(|(layout, _)| layout);
    let layout = stock::merged(state, own);
    let custom = &mut state.custom;
    // A waiting press belongs to the layout it was pressed on.
    if layout != custom.preset_layout {
        custom.gestures.clear();
        custom.preset_layout = layout;
    }
}

// ---- the mode ------------------------------------------------------------------------------

/// The stored ALL PRESETS layout, read at start-up (`None`: no file). A bad layout is reported
/// and the switches start empty.
pub fn load_layout(state: &mut State, layout: Option<Result<Vec<u8>, String>>) -> Vec<Effect> {
    let custom = &mut state.custom;
    custom.layout = empty_layout();
    custom.notice = None;
    let problem = match layout.map(|layout| layout.and_then(|bytes| decode(&bytes))) {
        None => None,
        Some(Ok((layout, dropped))) => {
            custom.layout = layout;
            (dropped > 0).then(|| format!("{dropped} slots were not valid"))
        }
        Some(Err(error)) => Some(format!("The layout was not read: {error}")),
    };
    custom.notice = problem.clone();
    problem
        .map(|problem| {
            Effect::Notice(
                super::Subsystem::Extensions,
                format!("{LAYOUT_FILE}: {problem}"),
            )
        })
        .into_iter()
        .collect()
}

/// Follows a change of the stored footswitch mode ([`plays_custom`]): on, the mode starts on
/// page 1; off, the held switches are released.
pub fn follow_mode(state: &mut State) -> Vec<Effect> {
    let on = plays_custom(state.settings.settings().mode.mode());
    if on == state.performance.footswitch.custom {
        return Vec::new();
    }
    let mut effects = match on {
        true => {
            state.custom.page = 0;
            state.custom.gestures.clear();
            state.performance.footswitch.overlay = None;
            Vec::new()
        }
        false => release_all(state, true),
    };
    state.performance.footswitch.custom = on;
    effects.push(Effect::Present);
    effects
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
        effects.extend(held.restore);
        for site in held.invert {
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
    let held = state.custom.held.iter().flatten();
    let own = (held.clone().flat_map(|held| &held.invert)).filter_map(|site| match site {
        Site::Slot(slot) => Some(*slot),
        _ => None,
    });
    // A held stock stomp in Momentary, as stock Stomp mode saves it (`preset::save_resting`).
    let stock = (state.current.as_ref()).into_iter().flat_map(|current| {
        let preset = &current.preset;
        (held.clone().filter_map(|held| held.stomp))
            .filter(|&stomp| preset.stomp_is_momentary.get(u32::from(stomp)) == Some(&true))
            .flat_map(|stomp| super::actions::stomp_slots(preset, u32::from(stomp)))
            .collect::<Vec<_>>()
    });
    own.chain(stock).collect()
}

/// A preset finished loading: the held momentary bypasses and parameters belonged to the old
/// preset's blocks; the other releases (Looper buttons, screens) still run.
pub fn preset_loaded(state: &mut State) {
    for held in state.custom.held.iter_mut().flatten() {
        held.invert.clear();
        held.restore.clear();
        // The stock release stays: stock skips it after a load (`perform.preset_loaded`).
        held.stomp = None;
    }
    state.custom.toggled = Default::default();
    state.custom.gestures.clear();
}

pub fn turn(state: &mut State, step: PageStep) -> Vec<Effect> {
    let pages = tetrad_ui::CUSTOM_PAGES;
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
    custom.playing_page(custom.page).shapes()
}

/// What the gestures fired, as one [`Action::CustomFire`] each: a press reads the state that the
/// presses before it left (a bypass, the page, the mode).
fn fire(state: &State, fired: Vec<Fire>, origin: Origin) -> Vec<Effect> {
    let epoch = state.custom.gestures.epoch();
    (fired.into_iter())
        .map(|fire| {
            let (slot, gesture) = match fire {
                Fire::Down { slot, gesture } => (slot, Some(gesture)),
                Fire::Up(slot) => (slot, None),
            };
            Effect::Action(Action::CustomFire {
                slot,
                gesture,
                epoch,
                origin,
            })
        })
        .collect()
}

/// One fired press or release ([`Action::CustomFire`]). A press is dropped when the waiting
/// presses were dropped since it fired (a page change, a preset load, a new layout, the end of
/// the mode).
pub fn fired(
    state: &mut State,
    slot: u8,
    gesture: Option<Gesture>,
    epoch: u32,
    origin: Origin,
) -> Vec<Effect> {
    sync(state);
    let ran = match gesture {
        Some(_)
            if !state.performance.footswitch.custom || state.custom.gestures.epoch() != epoch =>
        {
            None
        }
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
    let binding = (state.custom.playing_page(page))
        .binding(slot, gesture)?
        .clone();
    // A–H on page 1 play the preset's stock Stomp data first, through the stock stomp path.
    let stomp = (page == 0 && gesture == Gesture::Press && slot < stock::STOMPS).then_some(slot);
    let stocked = |pressed| {
        stomp.map_or_else(Vec::new, |stomp| {
            stock::press(state, &binding, stomp, pressed)
        })
    };
    let (mut effects, release) = (stocked(true), stocked(false));
    let stomp = stomp.filter(|_| !effects.is_empty());
    // Each action runs after the effects of the ones before ([`act`]).
    effects.extend(
        (0..binding.actions.len() as u8)
            .map(|at| Effect::Action(Action::CustomAct { slot, index: at })),
    );
    state.custom.held[index] = Some(Held {
        origin,
        page,
        gesture,
        release,
        restore: Vec::new(),
        invert: Vec::new(),
        actions: binding.actions,
        stomp,
    });
    Some(effects)
}

/// Runs action `index` of the press that `slot` holds, and keeps what its release owes. The
/// releases run in the opposite order of the presses. Nothing runs when the press has ended
/// (an action before it left the mode).
pub fn act(state: &mut State, slot: u8, index: u8) -> Vec<Effect> {
    let Some(held) = state
        .custom
        .held
        .get(usize::from(slot))
        .and_then(Option::as_ref)
    else {
        return Vec::new();
    };
    let Some(action) = held.actions.get(usize::from(index)).cloned() else {
        return Vec::new();
    };
    // The stock stomp path played it ([`start`]).
    if action.stock {
        return Vec::new();
    }
    let ran = run(state, (held.page, slot, held.gesture, index), &action);
    if let Some(held) = state.custom.held[usize::from(slot)].as_mut() {
        held.release.splice(0..0, ran.release);
        held.restore.splice(0..0, ran.restore);
        held.invert.extend(ran.invert);
    }
    ran.press
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

/// What one press of an action did, and what its release owes.
#[derive(Default)]
struct Ran {
    press: Vec<Effect>,
    release: Vec<Effect>,
    /// [`Held::restore`].
    restore: Vec<Effect>,
    /// The block of a momentary bypass ([`Held::invert`]).
    invert: Option<Site>,
}

impl Ran {
    fn press(press: Vec<Effect>) -> Ran {
        Ran {
            press,
            ..Ran::default()
        }
    }
}

/// One press of one action. `key` is the (page, slot, gesture, action index) of the action.
/// The mode decides as in Mappings: Momentary holds from the press to the release, the other
/// modes run on the press.
fn run(state: &mut State, key: (u8, u8, Gesture, u8), act: &Act) -> Ran {
    let act_on = |actions: Vec<Action>| actions.into_iter().map(Effect::Action).collect();
    let mode = act.mode();
    let held = matches!(mode, Mode::Momentary { .. });
    match &act.action {
        StoredAction::System { action } if !system_allowed(*action) => Ran::default(),
        StoredAction::System {
            action: SystemAction::CustomPage(step),
        } => Ran::press(turn(state, *step)),
        StoredAction::System {
            action: SystemAction::Looper(button),
        } => {
            let button = mappings::looper_button(*button);
            let edge = |pressed| Effect::Action(Action::Looper { button, pressed });
            match held {
                true => Ran {
                    press: vec![edge(true)],
                    release: vec![edge(false)],
                    ..Ran::default()
                },
                false => Ran::press(vec![edge(true), edge(false)]),
            }
        }
        StoredAction::System {
            action: action @ (SystemAction::Tuner | SystemAction::GigView),
        } => {
            let (screen, show): (Screen, fn(bool) -> Action) = match action {
                SystemAction::Tuner => (Screen::Tuner, Action::ShowTuner),
                _ => (Screen::Gig, Action::ShowGigView),
            };
            let on = state.screen == screen;
            let next = mode::pressed_switch(mode, on);
            if next == on {
                return Ran::default();
            }
            Ran {
                press: act_on(vec![show(next)]),
                release: match held {
                    true => act_on(vec![show(on)]),
                    false => Vec::new(),
                },
                ..Ran::default()
            }
        }
        StoredAction::System { action } => {
            let now = state.now;
            let mut tap = state.custom.last_tap;
            let actions = mappings::press(*action, state, now, &mut tap);
            state.custom.last_tap = tap;
            Ran::press(act_on(actions))
        }
        StoredAction::Block {
            row,
            column,
            model,
            target,
        } => {
            let Some(position) = mappings::position(*row, *column) else {
                return Ran::default();
            };
            let Some(model) = model.or_else(|| mappings::model_at(state, position)) else {
                return Ran::default();
            };
            block_press(state, position, model, *target, mode).unwrap_or_default()
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
                None => return Ran::default(),
            };
            Ran::press(vec![effect])
        }
        StoredAction::SendMidi => {
            let low = state.custom.toggled.contains(&key);
            let mut has_toggle = false;
            let effects = (act.messages.iter())
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
            Ran::press(effects)
        }
        StoredAction::Command { key, command } => Ran::press(vec![Effect::ExtensionCommand {
            key: key.clone(),
            command: command.clone(),
        }]),
        StoredAction::Nothing => Ran::default(),
    }
}

/// A press of a block action in `mode`. A momentary bypass writes as a stock momentary stomp
/// (source 3), and its release inverts the bypass as it then is; a momentary parameter's
/// release sets the value from before the press. `None` when the block is gone or replaced,
/// or the parameter is text or disabled.
fn block_press(
    state: &State,
    position: tetrad_ui::GridPosition,
    model: u32,
    target: StoredTarget,
    mode: Mode,
) -> Option<Ran> {
    let current = state.current.as_ref()?;
    let slot = mappings::engine_slot(position)?;
    let site = Site::Slot(slot);
    let instance = current.deployment.layout().instance(site)?;
    if instance.model.0 != model {
        return None;
    }
    let act = |action| vec![Effect::Action(action)];
    Some(match (target, mode) {
        (StoredTarget::Bypass, Mode::Momentary { .. }) => Ran {
            press: act(Action::MomentaryBypass {
                site,
                bypassed: !instance.bypass.at(current.deployment.scene()),
            }),
            invert: Some(site),
            ..Ran::default()
        },
        (StoredTarget::Bypass, mode) => {
            let engaged = !instance.bypass.at(current.deployment.scene());
            let next = mode::pressed_switch(mode, engaged);
            if next == engaged {
                return None;
            }
            Ran::press(act(Action::SetBypass {
                site,
                bypassed: !next,
            }))
        }
        (StoredTarget::Parameter { index }, mode) => {
            let param = mappings::param_at(state, slot, model, index)?;
            let value = param.pressed(mode)?;
            let set = |value| Action::SetParameter { site, index, value };
            Ran {
                press: act(set(value)),
                restore: match mode {
                    Mode::Momentary { .. } => act(set(param.current)),
                    _ => Vec::new(),
                },
                ..Ran::default()
            }
        }
    })
}

/// What an action drives, as the mode rules see it.
fn target(action: &StoredAction) -> Target {
    match action {
        StoredAction::Block {
            target: StoredTarget::Parameter { .. },
            ..
        } => Target::Parameter,
        StoredAction::Block { .. }
        | StoredAction::System {
            action: SystemAction::Tuner | SystemAction::GigView,
        } => Target::Switch,
        StoredAction::System {
            action: SystemAction::Looper(_),
        } => Target::Looper,
        StoredAction::SendMidi => Target::SendMidi,
        StoredAction::System { .. }
        | StoredAction::LoadPreset { .. }
        | StoredAction::Command { .. }
        | StoredAction::Nothing => Target::Command,
    }
}

/// The parameter a parameter action drives, while its block is there.
fn act_param<'a>(state: &'a State, act: &Act) -> Option<Param<'a>> {
    let StoredAction::Block {
        row,
        column,
        model: Some(model),
        target: StoredTarget::Parameter { index },
    } = act.action
    else {
        return None;
    };
    let slot = mappings::engine_slot(mappings::position(row, column)?)?;
    mappings::param_at(state, slot, model, index)
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
        CustomCommand::CopyPage { from, .. } if from >= tetrad_ui::CUSTOM_PAGES => {
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
        SlotEdit::Action { index, edit } => {
            let Some(edit) =
                action_edit(state, scope, (page, slot, gesture), index, edit, presets)?
            else {
                return Ok(Step::default());
            };
            let create = matches!(edit, ActionChange::Set(_));
            binding(
                Box::new(move |data| edit.apply(data, usize::from(index))),
                create,
            )
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
        layout[p].prune();
        Ok(())
    })
}

/// A checked action edit, ready to apply to the binding.
enum ActionChange {
    /// A new action at the index, or one more at the end.
    Set(StoredAction),
    Remove,
    /// The new mode of the action at the index, while it is still `action`.
    Mode {
        action: StoredAction,
        mode: Mode,
    },
    Messages(Vec<StoredMessage>),
}

impl ActionChange {
    fn apply(self, data: &mut Binding, index: usize) {
        match self {
            ActionChange::Set(action) => {
                let count = data.actions.len();
                match data.actions.get_mut(index) {
                    Some(act) => *act = Act::new(action),
                    None if index == count && index < MAX_ACTIONS => {
                        data.actions.push(Act::new(action));
                    }
                    None => {}
                }
            }
            ActionChange::Remove if index < data.actions.len() => {
                data.actions.remove(index);
            }
            ActionChange::Remove => {}
            ActionChange::Mode { action, mode } => {
                if let Some(act) = data
                    .actions
                    .get_mut(index)
                    .filter(|act| act.action == action)
                {
                    act.mode = Some(mode);
                }
            }
            ActionChange::Messages(messages) => {
                let act = data.actions.get_mut(index);
                if let Some(act) = act.filter(|act| act.action == StoredAction::SendMidi) {
                    act.messages = messages;
                }
            }
        }
    }
}

/// Checks one action edit against the layout of `scope` now; `None` when it changes nothing.
/// A new mode starts from the parameter's value now, as a new Mappings mode does.
fn action_edit(
    state: &State,
    scope: LayoutScope,
    (page, slot, gesture): (u8, u8, Gesture),
    index: u8,
    edit: ActionEdit,
    presets: PresetSlots,
) -> Result<Option<ActionChange>, String> {
    let custom = &state.custom;
    let layout = match scope {
        LayoutScope::Global => Some(&custom.layout),
        LayoutScope::Preset => custom.preset_layout.as_ref(),
    };
    let binding = (layout.and_then(|layout| layout.get(usize::from(page))))
        .and_then(|data| data.binding(slot, gesture));
    let count = binding.map_or(0, |binding| binding.actions.len());
    let act = binding.and_then(|binding| binding.actions.get(usize::from(index)));
    let change = match (edit, act) {
        (ActionEdit::Set(_), None) if usize::from(index) >= MAX_ACTIONS => {
            return Err(format!("A switch runs at most {MAX_ACTIONS} actions"));
        }
        (ActionEdit::Set(_), None) if usize::from(index) != count => {
            return Err("Add actions in order".into());
        }
        (ActionEdit::Set(action), _) => ActionChange::Set(stored_action(state, action, presets)?),
        (ActionEdit::Remove, Some(_)) => ActionChange::Remove,
        (_, None) => return Ok(None),
        (ActionEdit::Mode(kind), Some(act)) => {
            if !mode::modes(Source::Press, target(&act.action)).contains(&kind) {
                return Err(format!(
                    "{} does not fit this action",
                    mappings::mode_label(kind)
                ));
            }
            let mode = match act_param(state, act) {
                Some(param) => Mode::fresh(kind, param.current, param.other()),
                None => Mode::fresh(kind, 1.0, 0.0),
            };
            ActionChange::Mode {
                action: act.action.clone(),
                mode,
            }
        }
        (ActionEdit::Field { field, ticks }, Some(act)) => {
            let target = target(&act.action);
            let param = act_param(state, act);
            if target == Target::Parameter && param.is_none() {
                return Err("The block was moved or replaced: choose the action again".into());
            }
            let mut mode = act.mode();
            let mut window = Window::UPPER;
            let stepped = mode::fields(mode, Source::Press, target).contains(&field)
                && mode::step_field(&mut mode, &mut window, field, ticks, param.as_ref());
            if !stepped {
                return Ok(None);
            }
            ActionChange::Mode {
                action: act.action.clone(),
                mode,
            }
        }
        (ActionEdit::Messages(messages), Some(_)) => {
            if messages.len() > MAX_MESSAGES {
                return Err(format!("An action sends at most {MAX_MESSAGES} messages"));
            }
            ActionChange::Messages(messages.iter().map(stored_message).collect())
        }
    };
    Ok(Some(change))
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
    if touched
        .iter()
        .any(|(page, _)| *page >= tetrad_ui::CUSTOM_PAGES)
    {
        return Ok(Step::default());
    }
    let custom = &state.custom;
    let mut layout = match scope {
        LayoutScope::Global => custom.layout.clone(),
        LayoutScope::Preset => {
            (custom.preset_layout.clone()).ok_or_else(|| "No preset is loaded".to_owned())?
        }
    };
    let before = layout.clone();
    edit(&mut layout)?;
    if layout == before {
        return Ok(Step::default());
    }
    // A copy or move of a stock switch can bring more actions than a stored switch holds; only
    // page 1's A–H of THIS PRESET keep theirs in the stock data.
    let stored = |page: usize, slot: usize, gesture: Gesture| {
        scope == LayoutScope::Preset
            && page == 0
            && slot < usize::from(stock::STOMPS)
            && gesture == Gesture::Press
    };
    for (p, page) in layout.iter().enumerate() {
        let slots = page.slots.iter().enumerate().flat_map(|(s, slot)| {
            let press = std::iter::once((s, Gesture::Press, &slot.press));
            press.chain(
                slot.double
                    .iter()
                    .map(move |double| (s, Gesture::Double, double)),
            )
        });
        let combos = (page.combos.iter()).map(|combo| {
            (
                usize::from(combo.switches[0]),
                Gesture::Combo(0),
                &combo.binding,
            )
        });
        for (s, gesture, binding) in slots.chain(combos) {
            let kept = (binding.actions.iter())
                .filter(|act| !(act.stock && stored(p, s, gesture)))
                .count();
            if kept > MAX_ACTIONS {
                return Err(format!("A switch runs at most {MAX_ACTIONS} actions"));
            }
        }
    }
    // Both layouts play (switch by switch): held and waiting presses and CC Toggle states of
    // the touched switches end.
    let mut effects = Vec::new();
    state.custom.gestures.clear();
    let mut ended = Vec::new();
    for (page, slot) in &touched {
        let index = usize::from(*slot);
        ended.extend(state.custom.held[index].take_if(|held| held.page == *page));
        (state.custom.toggled).retain(|(p, s, ..)| (p, s) != (page, slot));
    }
    // One pass, so two held bypasses of one block end where they started.
    effects.extend(finish(state, ended));
    effects.push(Effect::Present);
    let save = match scope {
        LayoutScope::Global => {
            let bytes = encode(&layout);
            state.custom.layout = layout;
            Some((LAYOUT_FILE.to_owned(), bytes))
        }
        LayoutScope::Preset => {
            effects.extend(stock::store(state, &layout));
            state.custom.preset_layout = Some(layout);
            None
        }
    };
    Ok(Step { effects, save })
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
        SlotAction::Nothing => StoredAction::Nothing,
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

/// The slot editor and Gig View contents.
pub fn view(state: &State) -> Option<CustomModeView> {
    let custom = &state.custom;
    let views = |layout: &[Page]| {
        (0..tetrad_ui::CUSTOM_PAGES)
            .map(|page| match layout.get(usize::from(page)) {
                Some(data) => page_view(state, data, page),
                None => CustomPageView::default(),
            })
            .collect::<Vec<_>>()
    };
    let slots = views(&custom.layout);
    let preset_slots = custom.preset_layout.as_deref().map(views);
    let playing = (0..tetrad_ui::CUSTOM_PAGES)
        .map(|page| page_view(state, &custom.playing_page(page), page))
        .collect();
    Some(CustomModeView {
        name: NAME.to_owned(),
        extension: "Tetrad".to_owned(),
        pages: tetrad_ui::CUSTOM_PAGES,
        page: custom.page,
        active: state.performance.footswitch.custom,
        slots,
        preset_slots,
        playing,
        preset: (state.current.as_ref())
            .and_then(|current| current.preset.name.clone())
            .unwrap_or_default(),
        commands: custom.commands.clone(),
        colours: PALETTE,
        notice: custom.notice.clone(),
        double_press: custom.gestures.times().0,
        combo_press: custom.gestures.times().1,
    })
}

/// Settings › Double Press & Combo, as the settings view shows them.
pub fn settings(custom: &CustomMode) -> [(SettingId, SettingValue); 2] {
    let (double, combo) = custom.gestures.times();
    let index = |time: Duration, choices: &[u16]| {
        let at = choices
            .iter()
            .position(|&ms| u128::from(ms) == time.as_millis());
        SettingValue::Choice(at.unwrap_or_default() as u16)
    };
    [
        (
            SettingId::DoublePressTime,
            index(double, &DOUBLE_PRESS_CHOICES),
        ),
        (SettingId::ComboTime, index(combo, &COMBO_PRESS_CHOICES)),
    ]
}

/// A change of Settings › Double Press & Combo: the new time applies at once and is saved in
/// [`ACTIVE_FILE`]. `None` for any other setting.
pub fn change_setting(
    state: &mut State,
    id: SettingId,
    value: &SettingValue,
) -> Option<Result<Vec<Effect>, ActionError>> {
    let choices: &[u16] = match id {
        SettingId::DoublePressTime => &DOUBLE_PRESS_CHOICES,
        SettingId::ComboTime => &COMBO_PRESS_CHOICES,
        _ => return None,
    };
    let ms = match value {
        SettingValue::Choice(index) => choices.get(usize::from(*index)),
        _ => None,
    };
    let Some(&ms) = ms else {
        return Some(Err(ActionError::Unsupported("that time is not a choice")));
    };
    let time = Duration::from_millis(ms.into());
    let (double, combo) = state.custom.gestures.times();
    let (double, combo) = match id {
        SettingId::DoublePressTime => (time, combo),
        _ => (double, time),
    };
    state.custom.gestures.set_times(double, combo);
    Some(Ok(vec![Effect::CustomModeChanged]))
}

/// The LEDs of the current page: the press of each switch.
pub fn leds(state: &State) -> [Option<CustomLed>; CUSTOM_SLOTS] {
    let page = state.custom.page;
    std::array::from_fn(|index| {
        let playing = state.custom.playing_page(page);
        let data = &playing.slots[index];
        let first = data.press.actions.first()?;
        let at = (page, index as u8, Gesture::Press);
        let (_, _, colour, engaged, _) = describe(state, at, first, data.colour);
        Some(CustomLed {
            rgb: colour,
            engaged,
        })
    })
}

/// One page of a layout.
fn page_view(state: &State, data: &Page, page: u8) -> CustomPageView {
    let slots = std::array::from_fn(|index| {
        let slot = &data.slots[index];
        let at = |gesture| (page, index as u8, gesture);
        let double = (slot.double.as_ref())
            .map(|double| Box::new(binding_view(state, at(Gesture::Double), double, None)));
        CustomSlotView {
            double,
            ..binding_view(state, at(Gesture::Press), &slot.press, slot.colour)
        }
    });
    let combos = (data.combos.iter())
        .map(|combo| {
            let [first, second] = combo.switches;
            let at = (page, first, Gesture::Combo(second));
            CustomComboView {
                switches: combo.switches,
                binding: binding_view(state, at, &combo.binding, None),
            }
        })
        .collect();
    CustomPageView { slots, combos }
}

/// One press, double press or combo of a switch, with the switch's colour choice for its
/// press. The tile shows the first action.
fn binding_view(
    state: &State,
    at: (u8, u8, Gesture),
    data: &Binding,
    swatch: Option<u8>,
) -> CustomSlotView {
    let Some(first) = data.actions.first() else {
        return CustomSlotView {
            label: String::new(),
            detail: format!("{} · empty", SLOT_NAMES[usize::from(at.1)]),
            colour: [0; 3],
            available: true,
            ..CustomSlotView::default()
        };
    };
    let (label, detail, colour, engaged, available) = describe(state, at, first, swatch);
    let detail = match data.actions.len() {
        1 => detail,
        count => format!("{detail} · +{}", count - 1),
    };
    CustomSlotView {
        action: ui_action(&first.action),
        actions: (data.actions.iter())
            .filter_map(|act| action_view(state, at, act))
            .collect(),
        label: data.label.clone().unwrap_or(label),
        custom_label: data.label.clone(),
        detail,
        colour,
        swatch,
        engaged,
        available,
        double: None,
    }
}

/// One action as the editor shows it, with its mode as the Mappings page shows a row's.
fn action_view(state: &State, at: (u8, u8, Gesture), act: &Act) -> Option<CustomActionView> {
    let (label, detail, _, _, available) = describe(state, at, act, None);
    let param = act_param(state, act);
    let block = matches!(act.action, StoredAction::Block { .. });
    let target = (Source::Press, target(&act.action));
    let parts = mappings::mode_parts(act.mode(), Window::UPPER, target, param.as_ref(), block);
    Some(CustomActionView {
        action: ui_action(&act.action)?,
        label,
        detail,
        available,
        mode: Some(parts.mode),
        modes: parts.modes,
        fields: parts.fields,
        summary: parts.summary,
        messages: act.messages.iter().map(ui_message).collect(),
    })
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
        StoredAction::Nothing => SlotAction::Nothing,
    })
}

/// Label, detail, colour, engaged and available of an action at (page, slot, gesture). A held
/// press shows engaged.
fn describe(
    state: &State,
    (page, slot, gesture): (u8, u8, Gesture),
    data: &Act,
    swatch: Option<u8>,
) -> (String, String, [u8; 3], bool, bool) {
    let held = state.custom.held[usize::from(slot)]
        .as_ref()
        .is_some_and(|held| held.page == page && held.gesture == gesture);
    let swatch = swatch.map(|colour| PALETTE[usize::from(colour) % PALETTE.len()]);
    let current = state.current.as_ref();
    let action = &data.action;
    let (label, detail, colour, engaged, available) = match action {
        StoredAction::System { action } => {
            let (label, detail) = system_label(*action);
            // A scene that the loaded preset names shows its name, over "Scene A".
            let name = match *action {
                SystemAction::Scene(scene) => (current)
                    .and_then(|current| current.preset.scene_labels.get(usize::from(scene)))
                    .filter(|name| !name.is_empty()),
                _ => None,
            };
            let (label, detail) = match name {
                Some(name) => (name.clone(), label),
                None => (label, detail),
            };
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
                    let mode = mappings::mode_label(data.mode().kind());
                    (name, format!("{target_name} · {mode}"))
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
        StoredAction::Nothing => (
            "Nothing".into(),
            "Not the switch of all presets".into(),
            [0x3a; 3],
            false,
            true,
        ),
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

    /// The stored layout keeps every field of an assigned slot (its actions with their modes),
    /// its double press and the combos, and drops entries it cannot use.
    #[test]
    fn layouts_round_trip_and_drop_bad_entries() {
        let mut layout = empty_layout();
        let tuner = Binding {
            actions: vec![Act::new(StoredAction::System {
                action: SystemAction::Tuner,
            })],
            ..Binding::default()
        };
        let boost = Act {
            mode: Some(Mode::Toggle { a: 0.25, b: 0.6 }),
            ..Act::new(StoredAction::Block {
                row: 0,
                column: 2,
                model: Some(4004),
                target: StoredTarget::Parameter { index: 3 },
            })
        };
        let bypass = Act {
            mode: Some(Mode::Momentary { value: 0.0 }),
            ..Act::new(StoredAction::Block {
                row: 1,
                column: 0,
                model: None,
                target: StoredTarget::Bypass,
            })
        };
        layout[1].slots[9] = Slot {
            press: Binding {
                actions: vec![boost, bypass],
                label: Some("Lead boost".into()),
                ..Binding::default()
            },
            colour: Some(2),
            double: Some(tuner.clone()),
        };
        layout[3].slots[0].press = Binding {
            actions: vec![Act {
                messages: vec![StoredMessage {
                    kind: StoredKind::CcToggle,
                    channel: 3,
                    values: [Some(20), Some(0), Some(127)],
                }],
                ..Act::new(StoredAction::SendMidi)
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

        // A mode that does not fit its action, and a ninth action.
        let tuner = r#"{"action":{"kind":"system","action":"tuner"}}"#;
        let bad = format!(
            r#"{{"format":2,"slots":[
                {{"page":0,"slot":0,"actions":[{{"action":{{"kind":"system","action":"tuner"}},
                    "mode":{{"kind":"step","amount":1}}}}]}},
                {{"page":0,"slot":1,"actions":[{}]}}
            ]}}"#,
            [tuner; 9].join(",")
        );
        assert_eq!(decode(bad.as_bytes()).unwrap().1, 2);
        assert!(decode(br#"{"format":3,"slots":[]}"#).is_err());
    }

    /// A format 1 file reads as before: its one action is the first action, with the mode that
    /// does what it did. Entries it cannot use are dropped.
    #[test]
    fn format_1_layouts_upgrade() {
        let old = br#"{"format":1,"slots":[
            {"page":0,"slot":0,"action":{"kind":"block","row":0,"column":2,"model":4004,
                "target":{"kind":"parameter","index":3}},"value":80,"label":"Boost"},
            {"page":0,"slot":1,"action":{"kind":"block","row":0,"column":3,
                "target":{"kind":"bypass"}},"momentary":true},
            {"page":0,"slot":2,"action":{"kind":"system","action":{"looper":"playStop"}}},
            {"page":0,"slot":3,"action":{"kind":"sendMidi"},"messages":[
                {"kind":"pc","channel":1,"values":[null,null,12]}]}
        ]}"#;
        let (layout, dropped) = decode(old).unwrap();
        assert_eq!(dropped, 0);
        let first = |slot: usize| layout[0].slots[slot].press.actions[0].clone();
        assert_eq!(first(0).mode, Some(Mode::Set { value: 0.8 }));
        assert_eq!(layout[0].slots[0].press.label.as_deref(), Some("Boost"));
        let held = Some(Mode::fresh(ModeKind::Momentary, 1.0, 0.0));
        assert_eq!((first(1).mode, first(2).mode), (held, held));
        assert_eq!(first(3).messages.len(), 1);
        // Written as format 2, it reads back the same.
        assert_eq!(decode(&encode(&layout)).unwrap(), (layout, 0));

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
    }
}
