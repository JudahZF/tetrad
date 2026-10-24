//! Mappings bridge (owned, Tetrad): every trigger → action of the loaded preset and of the
//! device, behind the grid ⋮ menu's Mappings page (`tetrad_ui::mappings`).
//!
//! Rows come from two places. Stock data stays where stock keeps it: Stomp-mode assignments
//! (Stomp → block bypass), expression links (EXP → block) and Preset MIDI Out lists (Preset
//! loaded, Stomp or EXP → Send MIDI). Every other combination is a Tetrad mapping: per preset
//! in the preset field `TETRAD_MAPPINGS_FIELD`, globally in [`GLOBAL_FILE`]. An edit that turns
//! a row into a stock combination moves it to the stock data, and back.
//!
//! System actions run through the same device actions as the stock MIDI map (CC#43 scenes,
//! CC#45 tuner, …). Block actions are extension-style edits: stored, not reported back to the
//! extensions as touch edits.
//!
//! Each row has a value mode ([`mode`]): Set, Toggle, Momentary, Step, Follow, Relative or On
//! press. A row without a stored mode uses the first mode its trigger and action allow, which
//! is the behaviour from before modes.

mod mode;

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tetrad_core::performance::expression::Pedal;
use tetrad_engine::{Column, Row, SceneIndex, Site, Slot};
use tetrad_midi::MidiMessage;
use tetrad_preset::StompRole;
use tetrad_preset::model::TETRAD_MAPPINGS_FIELD;
use tetrad_settings::controls::FootswitchMode;
use tetrad_ui::{
    BlockTarget, ExtensionTriggerChoice, FieldId, Footswitch, GridPosition, LooperButton,
    MappingAction, MappingBlock, MappingCommand, MappingId, MappingParameter, MappingRow,
    MappingScope, MappingsView, MidiInput, MidiMessageKind, MidiOutMessage, MidiOutTarget,
    ModeChoice, ModeKind, ModeView, PresetMidiView, ResponseView, Screen, SystemAction, Trigger,
    ValueField,
};

use self::mode::{Input, Mode, Param, Source, Target, Window};
use super::extensions::FiredTrigger;
use super::preset::PresetEdit;
use super::{Action, Effect, PresetRef, State};

/// The device's global mappings.
pub const GLOBAL_FILE: &str = "/media/p4/tetrad/mappings.json";
/// Stored mappings per list; the file and the preset field are refused above it.
const MAX_MAPPINGS: usize = 128;
/// Mappings one trigger runs at most, so a crafted list cannot make one MIDI message cost
/// hundreds of preset edits.
const MAX_RUN: usize = 8;
/// Format 1 also covers `mode` and `window`: an older Tetrad ignores them.
const FORMAT: u32 = 1;
/// A CC refreshes the view (the Values page graph) at most this often.
const LEVEL_VIEW_INTERVAL: Duration = Duration::from_millis(50);

/// One Tetrad mapping as stored (JSON, [`StoredList`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    trigger: StoredTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action: Option<StoredAction>,
    /// Read only: the percent of a one-shot parameter action from before modes, which
    /// [`decode`] turns into [`Mode::Set`].
    #[serde(default, skip_serializing)]
    value: Option<u8>,
    /// `None`: the default mode of the trigger and action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<Mode>,
    /// CC triggers only; `None`: the mode's default window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window: Option<Window>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum StoredTrigger {
    PresetLoaded,
    Stomp {
        stomp: u8,
    },
    Expression {
        pedal: u8,
    },
    Cc {
        controller: u8,
    },
    Note {
        key: u8,
    },
    Extension {
        key: String,
        trigger: String,
        source: String,
        label: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum StoredAction {
    System {
        #[serde(with = "SystemDef")]
        action: SystemAction,
    },
    /// A grid block; `model` detects a replaced block, which the mapping then leaves alone.
    Block {
        row: u8,
        column: u8,
        model: u32,
        target: StoredTarget,
    },
    SendMidi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum StoredTarget {
    Bypass,
    Parameter { index: u32 },
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "SystemAction", rename_all = "camelCase")]
enum SystemDef {
    NextPreset,
    PreviousPreset,
    Scene(u8),
    NextScene,
    PreviousScene,
    Tuner,
    TapTempo,
    Mode(#[serde(with = "ModeDef")] ModeChoice),
    NextMode,
    GigView,
    Looper(#[serde(with = "LooperDef")] LooperButton),
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "ModeChoice", rename_all = "camelCase")]
enum ModeDef {
    Preset,
    Scene,
    Stomp,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "LooperButton", rename_all = "camelCase")]
enum LooperDef {
    RecordOverdub,
    PlayStop,
    Undo,
    Reverse,
    HalfSpeed,
    OneShot,
    PunchIn,
    Duplicate,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredList {
    format: u32,
    mappings: Vec<Stored>,
}

fn encode(mappings: &[Stored]) -> Vec<u8> {
    let list = StoredList {
        format: FORMAT,
        mappings: mappings.to_vec(),
    };
    serde_json::to_vec(&list).expect("mappings serialize")
}

/// A stored list of `scope`. Presets and the global file can come from anywhere (shared
/// presets, backups), so rows that the page could not make are dropped.
fn decode(bytes: &[u8], scope: MappingScope) -> Result<Vec<Stored>, String> {
    let mut list: StoredList = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if list.format != FORMAT {
        return Err(format!("format {} is not supported", list.format));
    }
    if list.mappings.len() > MAX_MAPPINGS {
        return Err(format!("more than {MAX_MAPPINGS} mappings"));
    }
    list.mappings.retain_mut(|stored| {
        if let Some(percent) = stored.value.take() {
            if percent > 100 {
                return false;
            }
            stored.mode = stored.mode.or(Some(Mode::Set {
                value: f32::from(percent) / 100.0,
            }));
        }
        trigger_allowed(scope, &stored.trigger).is_ok()
            && (stored.action.as_ref())
                .is_none_or(|action| action_allowed(scope, &stored.trigger, action).is_ok())
            && mode_allowed(stored).is_ok()
    });
    Ok(list.mappings)
}

/// A row of one list, for the runtime state of its mode.
type RowKey = (MappingScope, usize);

/// One input for one row, with the row's mode and CC window.
#[derive(Debug, Clone, Copy)]
struct Drive {
    key: RowKey,
    mode: Mode,
    window: Window,
    input: Input,
}

impl Stored {
    fn new(trigger: StoredTrigger, action: Option<StoredAction>) -> Stored {
        Stored {
            trigger,
            action,
            value: None,
            mode: None,
            window: None,
        }
    }
}

/// Work for the device after a call.
#[derive(Debug, Default)]
pub struct Step {
    pub effects: Vec<Effect>,
    /// New contents of [`GLOBAL_FILE`].
    pub save_global: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct Mappings {
    global: Vec<Stored>,
    /// The loaded preset's Tetrad mappings, decoded at `preset_revision`.
    preset: Vec<Stored>,
    preset_revision: Option<u64>,
    focus: Option<(MappingScope, MappingId)>,
    learning: Option<(MappingScope, MappingId)>,
    notice: Option<String>,
    last_tap: Option<Duration>,
    /// The last value of each CC trigger (extension labels left out): window edges, Pickup and
    /// the Values page graph.
    levels: HashMap<StoredTrigger, u8>,
    /// Momentary: the target's value from before the press, while held.
    held: HashMap<RowKey, f32>,
    /// The last value each parameter row set: Pickup has caught while the parameter keeps it.
    set: HashMap<RowKey, f32>,
    level_view: Option<Duration>,
    /// The global file was read (at the first preset load).
    pub(in crate::device) loaded: bool,
    /// Set by every step that reaches this bridge; the view is built again only then or when
    /// the preset changes.
    pub(in crate::device) view_stale: bool,
    view_revision: Option<u64>,
}

impl Mappings {
    /// The stored global mappings, read at start-up. A bad file is reported and ignored.
    pub fn load_global(&mut self, bytes: Option<&[u8]>) -> Option<String> {
        self.loaded = true;
        self.view_stale = true;
        match bytes.map(|bytes| decode(bytes, MappingScope::Global)) {
            None => None,
            Some(Ok(mappings)) => {
                self.global = mappings;
                None
            }
            Some(Err(error)) => Some(format!("{GLOBAL_FILE}: {error}")),
        }
    }

    /// Decodes the loaded preset's Tetrad mappings again after any change of the preset.
    fn sync(&mut self, state: &State) {
        let revision = state.current.revision();
        if self.preset_revision == Some(revision) {
            return;
        }
        self.preset_revision = Some(revision);
        self.preset = (state.current.as_ref())
            .and_then(|current| current.preset.unknown.bytes_field(TETRAD_MAPPINGS_FIELD))
            .and_then(|bytes| decode(&bytes, MappingScope::Preset).ok())
            .unwrap_or_default();
        let levels = std::mem::take(&mut self.levels);
        self.levels = (levels.into_iter())
            .filter(|(key, _)| self.mapped(key))
            .collect();
    }

    fn list(&self, scope: MappingScope) -> &Vec<Stored> {
        match scope {
            MappingScope::Preset => &self.preset,
            MappingScope::Global => &self.global,
        }
    }

    // ---- triggers ------------------------------------------------------------------------

    /// A native-channel MIDI message the stock map gives no job (`MidiOutput::Unclaimed`).
    pub fn on_midi(&mut self, message: &MidiMessage, state: &State, now: Duration) -> Step {
        self.sync(state);
        let (trigger, input) = match *message {
            MidiMessage::ControlChange {
                controller, value, ..
            } => (
                StoredTrigger::Cc {
                    controller: controller.get(),
                },
                Some(value.get()),
            ),
            MidiMessage::NoteOn { key, velocity, .. } if velocity.get() > 0 => {
                (StoredTrigger::Note { key: key.get() }, None)
            }
            MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                return self.fire(
                    StoredTrigger::Note { key: key.get() },
                    Input::Release,
                    state,
                    now,
                );
            }
            _ => return Step::default(),
        };
        let input = match input {
            Some(value) => self.level(&trigger, value, now),
            None => Input::Press,
        };
        self.fire(trigger, input, state, now)
    }

    /// A firing of an extension trigger.
    pub fn on_extension(&mut self, fired: &FiredTrigger, state: &State, now: Duration) -> Step {
        self.sync(state);
        let trigger = StoredTrigger::Extension {
            key: fired.key.clone(),
            trigger: fired.trigger.clone(),
            source: fired.source.clone(),
            label: fired.label.clone(),
        };
        let input = if fired.continuous {
            let value = (fired.value.clamp(0.0, 1.0) * 127.0).round() as u8;
            self.level(&trigger, value, now)
        } else if fired.pressed {
            Input::Press
        } else {
            Input::Release
        };
        self.fire(trigger, input, state, now)
    }

    /// A Stomp-mode footswitch press or release, 0..=7. Stock assignments toggle their blocks
    /// as before.
    pub fn on_stomp(
        &mut self,
        stomp: u8,
        pressed: bool,
        state: &State,
        now: Duration,
    ) -> Vec<Effect> {
        self.sync(state);
        let input = if pressed {
            Input::Press
        } else {
            Input::Release
        };
        let trigger = StoredTrigger::Stomp { stomp };
        self.run_matching(MappingScope::Preset, &trigger, input, state, now)
    }

    /// The preset finished loading. Held and set values of the old preset's rows go.
    pub fn on_preset_loaded(&mut self, state: &State, now: Duration) -> Vec<Effect> {
        self.sync(state);
        self.view_stale = true;
        self.learning = None;
        self.forget(MappingScope::Preset);
        self.run_matching(
            MappingScope::Preset,
            &StoredTrigger::PresetLoaded,
            Input::Press,
            state,
            now,
        )
    }

    /// Records the value of a mapped CC; the view follows at most every
    /// [`LEVEL_VIEW_INTERVAL`]. Unmapped CCs are not kept, so an extension cannot grow the map.
    fn level(&mut self, trigger: &StoredTrigger, value: u8, now: Duration) -> Input {
        let key = level_key(trigger);
        if !self.mapped(&key) {
            return Input::Level {
                value,
                previous: None,
            };
        }
        let previous = self.levels.insert(key, value);
        if (self.level_view).is_none_or(|last| now.saturating_sub(last) >= LEVEL_VIEW_INTERVAL) {
            self.level_view = Some(now);
            self.view_stale = true;
        }
        Input::Level { value, previous }
    }

    /// A row of either list has this trigger (a [`level_key`]).
    fn mapped(&self, key: &StoredTrigger) -> bool {
        (self.preset.iter().chain(&self.global)).any(|stored| level_key(&stored.trigger) == *key)
    }

    /// Runtime state of a list's rows: indexes move when the list changes.
    fn forget(&mut self, scope: MappingScope) {
        self.held.retain(|(s, _), _| *s != scope);
        self.set.retain(|(s, _), _| *s != scope);
    }

    /// LEARN takes the message (a native or an extension source becomes the row's trigger);
    /// else the preset's mappings for it run, or the global ones when the preset has none. A
    /// release never learns: letting go of a learned note must not learn again.
    fn fire(&mut self, trigger: StoredTrigger, input: Input, state: &State, now: Duration) -> Step {
        if input != Input::Release
            && let Some((scope, id)) = self.learning.take()
        {
            return self.set_trigger(scope, id, trigger, state);
        }
        let scope = if self.matching(MappingScope::Preset, &trigger).is_empty() {
            MappingScope::Global
        } else {
            MappingScope::Preset
        };
        Step {
            effects: self.run_matching(scope, &trigger, input, state, now),
            save_global: None,
        }
    }

    /// The rows of `scope` for `trigger`, with their indexes.
    fn matching(&self, scope: MappingScope, trigger: &StoredTrigger) -> Vec<(usize, Stored)> {
        let key = level_key(trigger);
        (self.list(scope).iter().enumerate())
            .filter(|(_, stored)| level_key(&stored.trigger) == key)
            .map(|(index, stored)| (index, stored.clone()))
            .collect()
    }

    fn run_matching(
        &mut self,
        scope: MappingScope,
        trigger: &StoredTrigger,
        input: Input,
        state: &State,
        now: Duration,
    ) -> Vec<Effect> {
        (self.matching(scope, trigger).iter())
            .take(MAX_RUN)
            .flat_map(|(index, stored)| self.run((scope, *index), stored, input, state, now))
            .collect()
    }

    // ---- actions --------------------------------------------------------------------------

    fn run(
        &mut self,
        key: RowKey,
        stored: &Stored,
        input: Input,
        state: &State,
        now: Duration,
    ) -> Vec<Effect> {
        let Some(action) = &stored.action else {
            return Vec::new();
        };
        let target = target(action);
        let mode = effective_mode(stored);
        let window = stored.window.unwrap_or(mode.default_window(target));
        // Follow and Relative read the CC value; the other modes read presses inside the window.
        let input = match (input, mode) {
            (Input::Level { .. }, Mode::Follow { .. } | Mode::Relative { .. }) => input,
            (Input::Level { value, previous }, _) => match mode::edge(value, previous, window) {
                Some(input) => input,
                None => return Vec::new(),
            },
            _ => input,
        };
        let drive = Drive {
            key,
            mode,
            window,
            input,
        };
        match action {
            StoredAction::System { action } => (self.system(drive, *action, state, now))
                .into_iter()
                .map(Effect::Action)
                .collect(),
            StoredAction::Block {
                row,
                column,
                model,
                target,
            } => (self.block(drive, state, (*row, *column, *model), *target))
                .map(Effect::ExtensionAction)
                .into_iter()
                .collect(),
            // The stock Preset MIDI Out runtime sends the lists.
            StoredAction::SendMidi => Vec::new(),
        }
    }

    /// The next state of a switch that is `on` now, or `None` when it stays.
    fn switch(&mut self, drive: Drive, on: bool) -> Option<bool> {
        let Drive {
            key,
            mode,
            window,
            input,
        } = drive;
        let next = match (input, mode) {
            (Input::Level { value, .. }, _) => window.contains(value),
            (Input::Press, Mode::Set { value }) => value >= 0.5,
            (Input::Press, Mode::Follow { .. }) => true,
            (Input::Press, Mode::Momentary { .. }) => {
                if self.held.contains_key(&key) {
                    return None;
                }
                self.held.insert(key, f32::from(u8::from(on)));
                !on
            }
            (Input::Press, _) => !on,
            (Input::Release, Mode::Momentary { .. }) => self.held.remove(&key)? >= 0.5,
            (Input::Release, Mode::Follow { .. }) => false,
            (Input::Release, _) => return None,
        };
        (next != on).then_some(next)
    }

    /// The next value of a parameter, or `None` when it stays.
    fn parameter(&mut self, drive: Drive, param: &Param) -> Option<f32> {
        let Drive {
            key,
            mode,
            window,
            input,
        } = drive;
        let current = param.current;
        let next = match (input, mode) {
            (Input::Level { value, previous }, Mode::Follow { min, max, pickup }) => {
                let next = mode::follow(value, window, min, max);
                if pickup && !self.caught(key, param) {
                    // Catch when the knob reaches the value, or moves across it.
                    let before = previous.map(|p| mode::follow(p, window, min, max));
                    let crossed =
                        before.is_some_and(|before| (before - current) * (next - current) <= 0.0);
                    if !crossed && !param.within_step(next, current) {
                        return None;
                    }
                }
                next
            }
            (Input::Level { value, .. }, Mode::Relative { encoding }) => {
                match mode::ticks(encoding, value) {
                    0 => return None,
                    ticks => param.spec.scale.encoder(current, ticks),
                }
            }
            (Input::Press, Mode::Set { value }) => value,
            (Input::Press, Mode::Follow { max, .. }) => max,
            (Input::Release, Mode::Follow { min, .. }) => min,
            (Input::Press, Mode::Toggle { a, b }) => {
                if param.near(current, a) {
                    b
                } else {
                    a
                }
            }
            (Input::Press, Mode::Momentary { value }) => {
                if self.held.contains_key(&key) {
                    return None;
                }
                self.held.insert(key, current);
                value
            }
            (Input::Release, Mode::Momentary { .. }) => self.held.remove(&key)?,
            (Input::Press, Mode::Step { amount, wrap }) => param.stepped(current, amount, wrap),
            _ => return None,
        }
        .clamp(0.0, 1.0);
        self.set.insert(key, next);
        ((next - current).abs() > f32::EPSILON).then_some(next)
    }

    /// Pickup has caught: the parameter still has the value this row set last.
    fn caught(&self, key: RowKey, param: &Param) -> bool {
        (self.set.get(&key)).is_some_and(|set| param.near(*set, param.current))
    }

    /// A block action for one input, or `None` when the block is gone or replaced, or the
    /// target stays as it is.
    fn block(
        &mut self,
        drive: Drive,
        state: &State,
        (row, column, model): (u8, u8, u32),
        target: StoredTarget,
    ) -> Option<Action> {
        let current = state.current.as_ref()?;
        let slot = engine_slot(position(row, column)?)?;
        let site = Site::Slot(slot);
        let instance = current.deployment.layout().instance(site)?;
        if instance.model.0 != model {
            return None;
        }
        let scene = current.deployment.scene();
        match target {
            StoredTarget::Bypass => {
                let engaged = !instance.bypass.at(scene);
                let next = self.switch(drive, engaged)?;
                Some(Action::SetBypass {
                    site,
                    bypassed: !next,
                })
            }
            StoredTarget::Parameter { index } => {
                let param = param_at(state, slot, model, index)?;
                let value = self.parameter(drive, &param)?;
                Some(Action::SetParameter { site, index, value })
            }
        }
    }

    fn system(
        &mut self,
        drive: Drive,
        action: SystemAction,
        state: &State,
        now: Duration,
    ) -> Vec<Action> {
        let scene = |step: i32| {
            let current = state.current.as_ref()?.deployment.scene().index();
            SceneIndex::new((current as i32 + step).rem_euclid(8) as u8).ok()
        };
        let preset = |step: i32| {
            let last = &state.settings.settings().last_preset;
            let index = u32::try_from(last.preset_index + step).ok()?;
            Some(Action::LoadPreset {
                target: PresetRef {
                    setlist: last.setlist.clone(),
                    index,
                },
                reload: false,
            })
        };
        match action {
            SystemAction::Tuner => (self.switch(drive, state.screen == Screen::Tuner))
                .map(Action::ShowTuner)
                .into_iter()
                .collect(),
            SystemAction::GigView => (self.switch(drive, state.screen == Screen::Gig))
                .map(Action::ShowGigView)
                .into_iter()
                .collect(),
            SystemAction::Looper(button) => {
                let button = looper_button(button);
                let press = |pressed| Action::Looper { button, pressed };
                match (drive.input, drive.mode) {
                    (Input::Press, Mode::Momentary { .. }) => vec![press(true)],
                    (Input::Release, Mode::Momentary { .. }) => vec![press(false)],
                    (Input::Press, _) => vec![press(true), press(false)],
                    _ => Vec::new(),
                }
            }
            _ if drive.input != Input::Press => Vec::new(),
            SystemAction::NextPreset => preset(1).into_iter().collect(),
            SystemAction::PreviousPreset => preset(-1).into_iter().collect(),
            SystemAction::Scene(index) => (SceneIndex::new(index).ok())
                .map(Action::SelectScene)
                .into_iter()
                .collect(),
            SystemAction::NextScene => scene(1).map(Action::SelectScene).into_iter().collect(),
            SystemAction::PreviousScene => scene(-1).map(Action::SelectScene).into_iter().collect(),
            SystemAction::TapTempo => {
                let interval = (self.last_tap).map(|last| now.saturating_sub(last).as_millis());
                self.last_tap = Some(now);
                match interval {
                    Some(ms) if super::ui::TAP_INTERVAL_MS.contains(&ms) => {
                        vec![Action::SetTempo(60_000.0 / ms as f32)]
                    }
                    _ => Vec::new(),
                }
            }
            SystemAction::Mode(mode) => vec![Action::SetMode(match mode {
                ModeChoice::Preset => FootswitchMode::Preset,
                ModeChoice::Scene => FootswitchMode::Scene,
                ModeChoice::Stomp => FootswitchMode::Stomp,
            })],
            SystemAction::NextMode => vec![Action::CycleMode],
        }
    }

    // ---- commands -------------------------------------------------------------------------

    pub fn command(&mut self, command: MappingCommand, state: &State) -> Step {
        self.sync(state);
        self.view_stale = true;
        self.notice = None;
        match command {
            MappingCommand::Add(scope) => self.add(scope, state),
            MappingCommand::SetTrigger { scope, id, trigger } => {
                let trigger = stored_trigger(trigger);
                self.set_trigger(scope, id, trigger, state)
            }
            MappingCommand::SetAction { scope, id, action } => match stored_action(state, action) {
                Ok(action) => self.set_action(scope, id, action, state),
                Err(reason) => self.refuse(reason),
            },
            MappingCommand::SetMode { scope, id, mode } => self.set_mode(scope, id, mode, state),
            MappingCommand::StepField {
                scope,
                id,
                field,
                ticks,
            } => self.step_field(scope, id, field, ticks, state),
            MappingCommand::Learn { scope, id, on } => {
                self.learning = on.then_some((scope, id));
                Step::default()
            }
            MappingCommand::Remove { scope, id } => self.remove(scope, &id, state),
        }
    }

    fn refuse(&mut self, reason: String) -> Step {
        self.notice = Some(reason);
        Step::default()
    }

    fn add(&mut self, scope: MappingScope, state: &State) -> Step {
        let mut list = self.list(scope).clone();
        if list.len() >= MAX_MAPPINGS {
            return self.refuse(format!("This list holds at most {MAX_MAPPINGS} mappings"));
        }
        let used = |trigger: &StoredTrigger| list.iter().any(|stored| &stored.trigger == trigger);
        let trigger = match scope {
            // The first footswitch with no assignment yet.
            MappingScope::Preset => (0..8)
                .map(|stomp| StoredTrigger::Stomp { stomp })
                .find(|trigger| !used(trigger) && !stock_stomp_used(state, trigger))
                .unwrap_or(StoredTrigger::Stomp { stomp: 0 }),
            MappingScope::Global => (3..120)
                .filter(|cc| !tetrad_midi::is_stock_controller(*cc))
                .map(|controller| StoredTrigger::Cc { controller })
                .find(|trigger| !used(trigger))
                .unwrap_or(StoredTrigger::Cc { controller: 3 }),
        };
        list.push(Stored::new(trigger, None));
        self.focus = Some((scope, MappingId::Tetrad(list.len() as u32 - 1)));
        self.store(scope, list)
    }

    /// The row as a Tetrad mapping: a stock row is taken out of the stock data first.
    fn take(
        &mut self,
        scope: MappingScope,
        id: &MappingId,
        state: &State,
    ) -> Option<(Stored, Vec<Effect>, Vec<Stored>)> {
        let mut list = self.list(scope).clone();
        match *id {
            MappingId::Tetrad(index) => {
                let index = index as usize;
                (index < list.len()).then(|| (list.remove(index), Vec::new(), list))
            }
            MappingId::Stomp { stomp, position } => {
                let model = model_at(state, position)?;
                let stored = Stored::new(
                    StoredTrigger::Stomp { stomp },
                    Some(StoredAction::Block {
                        row: position.row(),
                        column: position.column(),
                        model,
                        target: StoredTarget::Bypass,
                    }),
                );
                let effects = vec![Effect::Action(Action::PresetEdit(PresetEdit::Stomp {
                    slot: engine_slot(position)?,
                    assignment: None,
                }))];
                Some((stored, effects, list))
            }
            MappingId::Expression {
                pedal,
                position,
                target,
            } => {
                let model = model_at(state, position)?;
                let stored = Stored::new(
                    StoredTrigger::Expression { pedal },
                    Some(StoredAction::Block {
                        row: position.row(),
                        column: position.column(),
                        model,
                        target: stored_target(target),
                    }),
                );
                let effects = vec![link(position, target, pedal, false)?];
                Some((stored, effects, list))
            }
            MappingId::MidiOut(target) => {
                let stored = Stored::new(midi_out_trigger(target), Some(StoredAction::SendMidi));
                // Placeholders of the same list go with it.
                list.retain(|other| {
                    !(other.trigger == stored.trigger && other.action == stored.action)
                });
                let effects = vec![Effect::Action(Action::SetPresetMidi {
                    target,
                    messages: Vec::new(),
                })];
                Some((stored, effects, list))
            }
        }
    }

    fn set_trigger(
        &mut self,
        scope: MappingScope,
        id: MappingId,
        trigger: StoredTrigger,
        state: &State,
    ) -> Step {
        if let Err(reason) = trigger_allowed(scope, &trigger) {
            return self.refuse(reason);
        }
        let Some((mut stored, mut effects, list)) = self.take(scope, &id, state) else {
            return Step::default();
        };
        stored.trigger = trigger;
        if source(&stored.trigger) != Source::Level {
            stored.window = None;
        }
        if let Some(action) = &stored.action
            && let Err(reason) = action_allowed(scope, &stored.trigger, action)
        {
            stored.action = None;
            stored.mode = None;
            self.notice = Some(format!("Choose a new action: {reason}"));
        } else if mode_allowed(&stored).is_err() {
            stored.mode = None;
            let default = effective_mode(&stored).kind();
            self.notice = Some(format!("The mode went back to {}", mode_label(default)));
        }
        let mut step = self.place(scope, stored, list, state);
        effects.append(&mut step.effects);
        step.effects = effects;
        step
    }

    fn set_action(
        &mut self,
        scope: MappingScope,
        id: MappingId,
        action: StoredAction,
        state: &State,
    ) -> Step {
        let Some(trigger) = self.trigger_of(scope, &id) else {
            return Step::default();
        };
        if let Err(reason) = action_allowed(scope, &trigger, &action) {
            return self.refuse(reason);
        }
        let Some((mut stored, mut effects, list)) = self.take(scope, &id, state) else {
            return Step::default();
        };
        // A mode's values belong to its action: a new action starts at its default mode.
        if stored.action.as_ref() != Some(&action) {
            stored.mode = None;
        }
        stored.action = Some(action);
        let mut step = self.place(scope, stored, list, state);
        effects.append(&mut step.effects);
        step.effects = effects;
        step
    }

    /// A mode from the row's mode menu. On a stock Stomp → bypass row, Toggle and Momentary
    /// are the stock momentary flag of the footswitch; Set moves the row to Tetrad data.
    fn set_mode(
        &mut self,
        scope: MappingScope,
        id: MappingId,
        kind: ModeKind,
        state: &State,
    ) -> Step {
        let Some(stored) = self.row_of(scope, &id, state) else {
            return Step::default();
        };
        let modes = modes_of(&stored.trigger, stored.action.as_ref()).unwrap_or_default();
        if !modes.contains(&kind) {
            return self.refuse(format!("{} does not fit this mapping", mode_label(kind)));
        }
        let action = stored
            .action
            .as_ref()
            .expect("a row with modes has an action");
        let mode = match action_param(state, action) {
            Some(param) => Mode::fresh(kind, param.current, param.other()),
            None => Mode::fresh(kind, 1.0, 0.0),
        };
        match id {
            MappingId::Stomp { stomp, .. } if kind != ModeKind::Set => {
                self.focus = Some((scope, id));
                Step {
                    effects: vec![momentary_edit(stomp, mode)],
                    save_global: None,
                }
            }
            MappingId::Tetrad(index) if !stock_pair(&stored) => {
                let mut list = self.list(scope).clone();
                list[index as usize].mode = Some(mode);
                self.focus = Some((scope, id));
                self.store(scope, list)
            }
            _ => {
                let Some((mut stored, mut effects, list)) = self.take(scope, &id, state) else {
                    return Step::default();
                };
                stored.mode = Some(mode);
                let mut step = self.place(scope, stored, list, state);
                effects.append(&mut step.effects);
                step.effects = effects;
                step
            }
        }
    }

    /// A stepper tap on one of a Tetrad row's values.
    fn step_field(
        &mut self,
        scope: MappingScope,
        id: MappingId,
        field: FieldId,
        ticks: i32,
        state: &State,
    ) -> Step {
        let MappingId::Tetrad(index) = id else {
            return Step::default();
        };
        let mut list = self.list(scope).clone();
        let Some(stored) = list.get_mut(index as usize) else {
            return Step::default();
        };
        let Some(action) = &stored.action else {
            return Step::default();
        };
        let (source, target) = (source(&stored.trigger), target(action));
        let param = action_param(state, action);
        if target == Target::Parameter && param.is_none() {
            return self.refuse("The block was moved or replaced: choose the action again".into());
        }
        let mut mode = effective_mode(stored);
        let default_window = mode.default_window(target);
        let mut window = stored.window.unwrap_or(default_window);
        if !mode::fields(mode, source, target).contains(&field)
            || !mode::step_field(&mut mode, &mut window, field, ticks, param.as_ref())
        {
            return Step::default();
        }
        stored.mode = Some(mode);
        // The default window is not stored, so another mode gets its own default.
        stored.window = (window != default_window).then_some(window);
        self.focus = Some((scope, id));
        self.store(scope, list)
    }

    /// A row as Tetrad data would hold it, stock rows included.
    fn row_of(&self, scope: MappingScope, id: &MappingId, state: &State) -> Option<Stored> {
        match *id {
            MappingId::Tetrad(index) => self.list(scope).get(index as usize).cloned(),
            MappingId::Stomp { stomp, position } => {
                let mut stored = Stored::new(
                    StoredTrigger::Stomp { stomp },
                    Some(StoredAction::Block {
                        row: position.row(),
                        column: position.column(),
                        model: model_at(state, position)?,
                        target: StoredTarget::Bypass,
                    }),
                );
                stored.mode = Some(stomp_mode(state, stomp));
                Some(stored)
            }
            // Pedal links and Send MIDI lists have no modes to choose.
            MappingId::Expression { .. } | MappingId::MidiOut(_) => None,
        }
    }

    fn trigger_of(&self, scope: MappingScope, id: &MappingId) -> Option<StoredTrigger> {
        Some(match *id {
            MappingId::Tetrad(index) => self.list(scope).get(index as usize)?.trigger.clone(),
            MappingId::Stomp { stomp, .. } => StoredTrigger::Stomp { stomp },
            MappingId::Expression { pedal, .. } => StoredTrigger::Expression { pedal },
            MappingId::MidiOut(target) => midi_out_trigger(target),
        })
    }

    /// Stores a row where it belongs: stock data for stock combinations, else the Tetrad list.
    fn place(
        &mut self,
        scope: MappingScope,
        stored: Stored,
        mut list: Vec<Stored>,
        state: &State,
    ) -> Step {
        let stock = match (&stored.trigger, &stored.action) {
            (
                StoredTrigger::Stomp { stomp },
                Some(StoredAction::Block {
                    row,
                    column,
                    target: StoredTarget::Bypass,
                    ..
                }),
            ) if stock_pair(&stored) => position(*row, *column).and_then(|position| {
                let assign = Effect::Action(Action::PresetEdit(PresetEdit::Stomp {
                    slot: engine_slot(position)?,
                    assignment: Some((u32::from(*stomp), StompRole::Primary)),
                }));
                // A chosen Toggle or Momentary is the footswitch's stock momentary flag.
                let momentary = stored.mode.map(|mode| momentary_edit(*stomp, mode));
                Some((
                    std::iter::once(assign).chain(momentary).collect(),
                    MappingId::Stomp {
                        stomp: *stomp,
                        position,
                    },
                ))
            }),
            (
                StoredTrigger::Expression { pedal },
                Some(StoredAction::Block {
                    row,
                    column,
                    target,
                    ..
                }),
            ) => position(*row, *column).and_then(|position| {
                let target = ui_target(*target);
                let effect = link(position, target, *pedal, true)?;
                Some((
                    vec![effect],
                    MappingId::Expression {
                        pedal: *pedal,
                        position,
                        target,
                    },
                ))
            }),
            _ => None,
        };
        self.learning = None;
        match stock {
            Some((effects, id)) if scope == MappingScope::Preset => {
                self.focus = Some((scope, id));
                let mut step = self.store(scope, list);
                step.effects.splice(0..0, effects);
                step
            }
            _ => {
                // A Send MIDI row with stored messages shows as the stock list.
                let midi_out = match (&stored.trigger, &stored.action) {
                    (trigger, Some(StoredAction::SendMidi)) => midi_out_target(trigger)
                        .filter(|target| !preset_midi(state).messages(*target).is_empty()),
                    _ => None,
                };
                list.push(stored);
                self.focus = Some((
                    scope,
                    match midi_out {
                        Some(target) => MappingId::MidiOut(target),
                        None => MappingId::Tetrad(list.len() as u32 - 1),
                    },
                ));
                self.store(scope, list)
            }
        }
    }

    fn remove(&mut self, scope: MappingScope, id: &MappingId, state: &State) -> Step {
        let Some((_, effects, list)) = self.take(scope, id, state) else {
            return Step::default();
        };
        self.focus = None;
        self.learning = None;
        let mut step = self.store(scope, list);
        step.effects.splice(0..0, effects);
        step
    }

    /// Writes a scope's Tetrad list: the preset field (a stored, undoable preset edit) or the
    /// global file.
    fn store(&mut self, scope: MappingScope, list: Vec<Stored>) -> Step {
        self.view_stale = true;
        self.forget(scope);
        let bytes = (!list.is_empty()).then(|| encode(&list));
        match scope {
            MappingScope::Preset => {
                let unchanged = list == self.preset;
                self.preset = list;
                Step {
                    effects: (!unchanged)
                        .then(|| Effect::Action(Action::PresetEdit(PresetEdit::Mappings(bytes))))
                        .into_iter()
                        .collect(),
                    save_global: None,
                }
            }
            MappingScope::Global => {
                self.global = list;
                Step {
                    effects: Vec::new(),
                    save_global: Some(encode(&self.global)),
                }
            }
        }
    }

    // ---- view -----------------------------------------------------------------------------

    /// The view when it may have changed: after a step here or any change of the preset.
    pub fn view_if_changed(
        &mut self,
        state: &State,
        triggers: impl FnOnce() -> Vec<ExtensionTriggerChoice>,
    ) -> Option<MappingsView> {
        let revision = state.current.revision();
        if !std::mem::take(&mut self.view_stale) && self.view_revision == Some(revision) {
            return None;
        }
        self.view_revision = Some(revision);
        self.sync(state);
        Some(self.view(state, triggers()))
    }

    pub fn view(
        &self,
        state: &State,
        extension_triggers: Vec<ExtensionTriggerChoice>,
    ) -> MappingsView {
        let midi = preset_midi(state);
        let mut preset = stock_rows(self, state, &midi);
        let stock_lists: Vec<StoredTrigger> = (preset.iter())
            .filter(|(_, row)| matches!(row.id, MappingId::MidiOut(_)))
            .map(|(trigger, _)| trigger.clone())
            .collect();
        preset.extend(
            self.preset
                .iter()
                .enumerate()
                .filter(|(_, stored)| {
                    // A Send MIDI placeholder hides once its stock list has messages.
                    !(stored.action == Some(StoredAction::SendMidi)
                        && stock_lists.contains(&stored.trigger))
                })
                .map(|(index, stored)| {
                    let row = self.row(
                        state,
                        &midi,
                        &extension_triggers,
                        MappingId::Tetrad(index as u32),
                        stored,
                        Some((MappingScope::Preset, index)),
                    );
                    (stored.trigger.clone(), row)
                }),
        );
        preset.sort_by_key(|(trigger, _)| rank(trigger));
        let global = (self.global.iter().enumerate())
            .map(|(index, stored)| {
                self.row(
                    state,
                    &midi,
                    &extension_triggers,
                    MappingId::Tetrad(index as u32),
                    stored,
                    Some((MappingScope::Global, index)),
                )
            })
            .collect();
        MappingsView {
            preset_name: (state.current.as_ref())
                .and_then(|current| current.preset.name.clone())
                .unwrap_or_default(),
            preset: preset.into_iter().map(|(_, row)| row).collect(),
            global,
            blocks: blocks(state),
            extension_triggers,
            focus: self.focus.clone(),
            learning: self.learning.clone(),
            notice: self.notice.clone(),
        }
    }
}

// ---- rules ---------------------------------------------------------------------------------

fn source(trigger: &StoredTrigger) -> Source {
    match trigger {
        StoredTrigger::PresetLoaded => Source::Load,
        StoredTrigger::Stomp { .. } | StoredTrigger::Note { .. } => Source::Press,
        StoredTrigger::Expression { .. } => Source::Pedal,
        StoredTrigger::Cc { .. } => Source::Level,
        StoredTrigger::Extension { source, .. } if source.starts_with("cc:") => Source::Level,
        StoredTrigger::Extension { .. } => Source::Press,
    }
}

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
        StoredAction::System { .. } => Target::Command,
        StoredAction::SendMidi => Target::SendMidi,
    }
}

/// The trigger without its display label: rows and CC values match on this.
fn level_key(trigger: &StoredTrigger) -> StoredTrigger {
    match trigger {
        StoredTrigger::Extension {
            key,
            trigger,
            source,
            ..
        } => StoredTrigger::Extension {
            key: key.clone(),
            trigger: trigger.clone(),
            source: source.clone(),
            label: String::new(),
        },
        other => other.clone(),
    }
}

/// The modes of a row's trigger and action; `None` without an action.
fn modes_of(trigger: &StoredTrigger, action: Option<&StoredAction>) -> Option<&'static [ModeKind]> {
    Some(mode::modes(source(trigger), target(action?)))
}

/// The stored mode, or the default of the trigger and action (the behaviour from before
/// modes). A pedal row has no modes and uses Follow.
fn effective_mode(stored: &Stored) -> Mode {
    stored.mode.unwrap_or_else(|| {
        let kind = modes_of(&stored.trigger, stored.action.as_ref())
            .and_then(|modes| modes.first().copied())
            .unwrap_or(ModeKind::Follow);
        Mode::fresh(kind, 1.0, 0.0)
    })
}

/// A stored mode and window fit the row's trigger and action.
fn mode_allowed(stored: &Stored) -> Result<(), String> {
    if let Some(mode) = stored.mode {
        let modes = modes_of(&stored.trigger, stored.action.as_ref()).unwrap_or_default();
        if !modes.contains(&mode.kind()) || !mode.valid() {
            return Err("this mode does not fit the trigger and action".into());
        }
    }
    match stored.window {
        Some(window) if !window.valid() => Err("the CC window goes from 0 to 127".into()),
        Some(_) if source(&stored.trigger) != Source::Level => {
            Err("only CC triggers have a window".into())
        }
        _ => Ok(()),
    }
}

fn trigger_allowed(scope: MappingScope, trigger: &StoredTrigger) -> Result<(), String> {
    match trigger {
        StoredTrigger::Cc { controller } if tetrad_midi::is_stock_controller(*controller) => Err(
            format!("CC {controller} is used by the QC's MIDI map: choose another"),
        ),
        StoredTrigger::Cc { controller } | StoredTrigger::Note { key: controller }
            if *controller > 127 =>
        {
            Err("MIDI numbers go from 0 to 127".into())
        }
        StoredTrigger::Stomp { stomp } if *stomp > 7 => Err("Footswitches go from A to H".into()),
        StoredTrigger::Expression { pedal } if !matches!(pedal, 1 | 2) => {
            Err("The pedals are EXP 1 and EXP 2".into())
        }
        StoredTrigger::PresetLoaded
        | StoredTrigger::Stomp { .. }
        | StoredTrigger::Expression { .. }
            if scope == MappingScope::Global =>
        {
            Err("Global mappings use MIDI triggers; this trigger belongs to a preset".into())
        }
        _ => Ok(()),
    }
}

fn action_allowed(
    scope: MappingScope,
    trigger: &StoredTrigger,
    action: &StoredAction,
) -> Result<(), String> {
    match action {
        StoredAction::Block { .. } if scope == MappingScope::Global => {
            Err("Block actions are saved in a preset: use This preset".into())
        }
        StoredAction::SendMidi if midi_out_target(trigger).is_none() => {
            Err("Send MIDI works with Preset loaded, footswitch and pedal triggers".into())
        }
        StoredAction::SendMidi | StoredAction::Block { .. } => Ok(()),
        StoredAction::System { .. } if matches!(trigger, StoredTrigger::Expression { .. }) => {
            Err("An expression pedal drives block parameters, bypass or Send MIDI".into())
        }
        // Two presets that load each other on load would never stop.
        StoredAction::System {
            action: SystemAction::NextPreset | SystemAction::PreviousPreset,
        } if *trigger == StoredTrigger::PresetLoaded => {
            Err("A preset load cannot load another preset".into())
        }
        StoredAction::System { .. } => Ok(()),
    }
}

/// Stomp → bypass in a mode that stock data holds: Toggle or Momentary (the footswitch's
/// momentary flag).
fn stock_pair(stored: &Stored) -> bool {
    matches!(
        (&stored.trigger, &stored.action, stored.mode),
        (
            StoredTrigger::Stomp { .. },
            Some(StoredAction::Block {
                target: StoredTarget::Bypass,
                ..
            }),
            None | Some(Mode::Toggle { .. } | Mode::Momentary { .. })
        )
    )
}

/// The mode of a stock Stomp → bypass row: the footswitch's momentary flag.
fn stomp_mode(state: &State, stomp: u8) -> Mode {
    let momentary = (state.current.as_ref())
        .and_then(|current| {
            current
                .preset
                .stomp_is_momentary
                .get(u32::from(stomp))
                .copied()
        })
        .unwrap_or(false);
    let kind = if momentary {
        ModeKind::Momentary
    } else {
        ModeKind::Toggle
    };
    Mode::fresh(kind, 1.0, 0.0)
}

fn momentary_edit(stomp: u8, mode: Mode) -> Effect {
    Effect::Action(Action::PresetEdit(PresetEdit::Momentary {
        stomp: u32::from(stomp),
        momentary: matches!(mode, Mode::Momentary { .. }),
    }))
}

fn stock_stomp_used(state: &State, trigger: &StoredTrigger) -> bool {
    let StoredTrigger::Stomp { stomp } = trigger else {
        return false;
    };
    state.current.as_ref().is_some_and(|current| {
        (current.preset.stomp_mode_assignments.iter()).any(|a| a.stomp_index == u32::from(*stomp))
    })
}

fn rank(trigger: &StoredTrigger) -> (u8, u8) {
    match trigger {
        StoredTrigger::PresetLoaded => (0, 0),
        StoredTrigger::Stomp { stomp } => (1, *stomp),
        StoredTrigger::Expression { pedal } => (2, *pedal),
        StoredTrigger::Cc { .. } | StoredTrigger::Note { .. } => (3, 0),
        StoredTrigger::Extension { .. } => (4, 0),
    }
}

// ---- conversions ---------------------------------------------------------------------------

fn stored_trigger(trigger: Trigger) -> StoredTrigger {
    match trigger {
        Trigger::PresetLoaded => StoredTrigger::PresetLoaded,
        Trigger::Stomp(stomp) => StoredTrigger::Stomp { stomp },
        Trigger::Expression(pedal) => StoredTrigger::Expression { pedal },
        Trigger::Midi(MidiInput::Cc(controller)) => StoredTrigger::Cc { controller },
        Trigger::Midi(MidiInput::Note(key)) => StoredTrigger::Note { key },
        Trigger::Extension {
            key,
            trigger,
            source,
        } => StoredTrigger::Extension {
            label: source_label(&source),
            key,
            trigger,
            source,
        },
    }
}

fn ui_trigger(trigger: &StoredTrigger) -> Trigger {
    match trigger {
        StoredTrigger::PresetLoaded => Trigger::PresetLoaded,
        StoredTrigger::Stomp { stomp } => Trigger::Stomp(*stomp),
        StoredTrigger::Expression { pedal } => Trigger::Expression(*pedal),
        StoredTrigger::Cc { controller } => Trigger::Midi(MidiInput::Cc(*controller)),
        StoredTrigger::Note { key } => Trigger::Midi(MidiInput::Note(*key)),
        StoredTrigger::Extension {
            key,
            trigger,
            source,
            ..
        } => Trigger::Extension {
            key: key.clone(),
            trigger: trigger.clone(),
            source: source.clone(),
        },
    }
}

/// "cc:20" → "CC 20"; other sources show as they are.
fn source_label(source: &str) -> String {
    match source.split_once(':') {
        Some(("cc", n)) => format!("CC {n}"),
        Some(("note", n)) => format!("Note {n}"),
        Some(("pc", n)) => n.parse::<u16>().map_or_else(
            |_| source.to_owned(),
            |n| format!("PC {}", n.saturating_add(1)),
        ),
        _ => source.to_owned(),
    }
}

fn stored_action(state: &State, action: MappingAction) -> Result<StoredAction, String> {
    Ok(match action {
        MappingAction::System(action) => StoredAction::System { action },
        MappingAction::SendMidi => StoredAction::SendMidi,
        MappingAction::Block { position, target } => StoredAction::Block {
            row: position.row(),
            column: position.column(),
            model: model_at(state, position).ok_or("That grid slot holds no block")?,
            target: stored_target(target),
        },
    })
}

fn ui_action(action: &StoredAction) -> Option<MappingAction> {
    Some(match action {
        StoredAction::System { action } => MappingAction::System(*action),
        StoredAction::SendMidi => MappingAction::SendMidi,
        StoredAction::Block {
            row,
            column,
            target,
            ..
        } => MappingAction::Block {
            position: position(*row, *column)?,
            target: ui_target(*target),
        },
    })
}

fn stored_target(target: BlockTarget) -> StoredTarget {
    match target {
        BlockTarget::Bypass => StoredTarget::Bypass,
        BlockTarget::Parameter(index) => StoredTarget::Parameter { index },
    }
}

fn ui_target(target: StoredTarget) -> BlockTarget {
    match target {
        StoredTarget::Bypass => BlockTarget::Bypass,
        StoredTarget::Parameter { index } => BlockTarget::Parameter(index),
    }
}

fn looper_button(button: LooperButton) -> tetrad_midi::LooperButton {
    use tetrad_midi::LooperButton as L;
    match button {
        LooperButton::RecordOverdub => L::RecordOverdub,
        LooperButton::PlayStop => L::PlayStop,
        LooperButton::Undo => L::Undo,
        LooperButton::Reverse => L::Reverse,
        LooperButton::HalfSpeed => L::HalfSpeed,
        LooperButton::OneShot => L::OneShot,
        LooperButton::PunchIn => L::PunchIn,
        LooperButton::Duplicate => L::Duplicate,
    }
}

fn midi_out_target(trigger: &StoredTrigger) -> Option<MidiOutTarget> {
    match trigger {
        StoredTrigger::PresetLoaded => Some(MidiOutTarget::OnLoad),
        StoredTrigger::Stomp { stomp } => Footswitch::TILES
            .get(usize::from(*stomp))
            .map(|switch| MidiOutTarget::Footswitch(*switch)),
        StoredTrigger::Expression { pedal: 1 } => Some(MidiOutTarget::Expression1),
        StoredTrigger::Expression { pedal: 2 } => Some(MidiOutTarget::Expression2),
        _ => None,
    }
}

fn midi_out_trigger(target: MidiOutTarget) -> StoredTrigger {
    match target {
        MidiOutTarget::OnLoad => StoredTrigger::PresetLoaded,
        MidiOutTarget::Footswitch(switch) => StoredTrigger::Stomp {
            stomp: switch.tile().unwrap_or(0) as u8,
        },
        MidiOutTarget::Expression1 => StoredTrigger::Expression { pedal: 1 },
        MidiOutTarget::Expression2 => StoredTrigger::Expression { pedal: 2 },
    }
}

fn position(row: u8, column: u8) -> Option<GridPosition> {
    GridPosition::new(row, column).ok()
}

fn engine_slot(position: GridPosition) -> Option<Slot> {
    Some(Slot {
        row: Row::new(position.row()).ok()?,
        column: Column::new(position.column()).ok()?,
    })
}

fn model_at(state: &State, position: GridPosition) -> Option<u32> {
    let current = state.current.as_ref()?;
    let instance = current
        .deployment
        .layout()
        .instance(Site::Slot(engine_slot(position)?))?;
    Some(instance.model.0)
}

fn link(position: GridPosition, target: BlockTarget, pedal: u8, linked: bool) -> Option<Effect> {
    Some(Effect::Action(Action::LinkExpression {
        slot: engine_slot(position)?,
        parameter: match target {
            BlockTarget::Bypass => None,
            BlockTarget::Parameter(index) => Some(index),
        },
        pedal: if pedal == 1 { Pedal::Exp1 } else { Pedal::Exp2 },
        linked,
    }))
}

/// The parameter at `index` of the block in `slot`, when the page offers it (no text or
/// disabled parameters).
fn param_at(state: &State, slot: Slot, model: u32, index: u32) -> Option<Param<'_>> {
    let current = state.current.as_ref()?;
    let instance = current.deployment.layout().instance(Site::Slot(slot))?;
    if instance.model.0 != model {
        return None;
    }
    let spec = state.specs.as_ref()?.model(model)?.parameter(index)?;
    if spec.kind == tetrad_catalog::ParameterType::String || spec.scale.disabled {
        return None;
    }
    let current = instance
        .params
        .get(index as usize)?
        .at(current.deployment.scene());
    Some(Param { spec, current })
}

/// The parameter a block action drives, when its block is still there.
fn action_param<'a>(state: &'a State, action: &StoredAction) -> Option<Param<'a>> {
    let StoredAction::Block {
        row,
        column,
        model,
        target: StoredTarget::Parameter { index },
    } = *action
    else {
        return None;
    };
    param_at(state, engine_slot(position(row, column)?)?, model, index)
}

fn preset_midi(state: &State) -> PresetMidiView {
    (state.current.as_ref())
        .map(|current| super::ui::project::preset_midi(&current.preset))
        .unwrap_or_default()
}

// ---- rows ----------------------------------------------------------------------------------

fn block_name(state: &State, position: GridPosition) -> Option<String> {
    let model = model_at(state, position)?;
    let specs = state.specs.as_ref()?;
    Some(specs.model(model)?.name.clone())
}

fn target_name(state: &State, position: GridPosition, target: BlockTarget) -> String {
    match target {
        BlockTarget::Bypass => "Bypass".into(),
        BlockTarget::Parameter(index) => (model_at(state, position))
            .and_then(|model| state.specs.as_ref()?.model(model)?.parameter(index))
            .map_or_else(
                || format!("Parameter {}", index.saturating_add(1)),
                |spec| spec.name.clone(),
            ),
    }
}

fn stomp_letter(stomp: u8) -> char {
    char::from(b'A' + stomp.min(7))
}

fn trigger_text(
    trigger: &StoredTrigger,
    extension_triggers: &[ExtensionTriggerChoice],
) -> (String, String) {
    match trigger {
        StoredTrigger::PresetLoaded => {
            ("Preset loaded".into(), "Each time this preset loads".into())
        }
        StoredTrigger::Stomp { stomp } => (
            format!("Stomp {}", stomp_letter(*stomp)),
            "Footswitch press".into(),
        ),
        StoredTrigger::Expression { pedal } => (format!("EXP {pedal}"), "Expression pedal".into()),
        StoredTrigger::Cc { controller } => {
            (format!("CC {controller}"), "MIDI · QC channel".into())
        }
        StoredTrigger::Note { key } => (format!("Note {key}"), "MIDI · QC channel".into()),
        StoredTrigger::Extension {
            key,
            trigger,
            source,
            label,
        } => {
            let declared = (extension_triggers.iter())
                .find(|choice| &choice.key == key && &choice.trigger == trigger)
                .map_or_else(
                    || format!("{key} (not running)"),
                    |choice| choice.label.clone(),
                );
            if source.is_empty() {
                (declared, "Not learned yet".into())
            } else {
                (label.clone(), declared)
            }
        }
    }
}

fn message_text(message: &MidiOutMessage) -> String {
    let value = |index: usize| message.values[index].map_or_else(|| "-".into(), |v| v.to_string());
    match message.kind {
        MidiMessageKind::Cc => format!("CC {} = {} · Ch {}", value(0), value(1), message.channel),
        MidiMessageKind::CcToggle => format!("CC {} toggle · Ch {}", value(0), message.channel),
        MidiMessageKind::Pc => format!("PC {} · Ch {}", value(2), message.channel),
    }
}

fn action_text(
    state: &State,
    midi: &PresetMidiView,
    trigger: &StoredTrigger,
    action: &StoredAction,
) -> (String, String) {
    match action {
        StoredAction::System { action } => system_text(*action),
        StoredAction::Block {
            row,
            column,
            model,
            target,
        } => match position(*row, *column) {
            Some(position) if model_at(state, position) == Some(*model) => (
                block_name(state, position).unwrap_or_else(|| "Block".into()),
                target_name(state, position, ui_target(*target)),
            ),
            _ => (
                "Missing block".into(),
                "The block was moved or replaced".into(),
            ),
        },
        StoredAction::SendMidi => {
            let messages = midi_out_target(trigger)
                .map(|target| midi.messages(target))
                .unwrap_or_default();
            let detail = match messages {
                [] => "No messages yet: tap Edit messages".into(),
                [one] => message_text(one),
                [first, rest @ ..] => format!("{} +{}", message_text(first), rest.len()),
            };
            ("Send MIDI".into(), detail)
        }
    }
}

fn system_text(action: SystemAction) -> (String, String) {
    let (label, detail): (&str, String) = match action {
        SystemAction::NextPreset => ("Preset", "Next preset".into()),
        SystemAction::PreviousPreset => ("Preset", "Previous preset".into()),
        SystemAction::Scene(scene) => ("Scene", format!("Scene {}", stomp_letter(scene))),
        SystemAction::NextScene => ("Scene", "Next scene".into()),
        SystemAction::PreviousScene => ("Scene", "Previous scene".into()),
        SystemAction::Tuner => ("Tuner", "Open and close".into()),
        SystemAction::TapTempo => ("Tempo", "Tap tempo".into()),
        SystemAction::Mode(mode) => (
            "Mode",
            match mode {
                ModeChoice::Preset => "Preset mode",
                ModeChoice::Scene => "Scene mode",
                ModeChoice::Stomp => "Stomp mode",
            }
            .into(),
        ),
        SystemAction::NextMode => ("Mode", "Next mode".into()),
        SystemAction::GigView => ("Gig View", "Open and close".into()),
        SystemAction::Looper(button) => (
            "Looper",
            match button {
                LooperButton::RecordOverdub => "Rec / Overdub",
                LooperButton::PlayStop => "Play / Stop",
                LooperButton::Undo => "Undo",
                LooperButton::Reverse => "Reverse",
                LooperButton::HalfSpeed => "Half speed",
                LooperButton::OneShot => "One shot",
                LooperButton::PunchIn => "Punch in",
                LooperButton::Duplicate => "Duplicate",
            }
            .into(),
        ),
    };
    (label.into(), detail)
}

fn mode_label(kind: ModeKind) -> &'static str {
    match kind {
        ModeKind::OnPress => "On press",
        ModeKind::Follow => "Follow",
        ModeKind::Relative => "Relative",
        ModeKind::Set => "Set",
        ModeKind::Toggle => "Toggle",
        ModeKind::Momentary => "Momentary",
        ModeKind::Step => "Step",
    }
}

fn mode_view(kind: ModeKind, source: Source, target: Target) -> ModeView {
    let detail = match (kind, target, source) {
        (ModeKind::OnPress, Target::SendMidi, _) => "Sends the messages on each trigger",
        (ModeKind::OnPress, _, Source::Load) => "Runs when the preset loads",
        (ModeKind::OnPress, _, Source::Level) => "Runs when the CC enters its window",
        (ModeKind::OnPress, Target::Looper, _) => "Taps the button on each press",
        (ModeKind::OnPress, ..) => "Runs on each press",
        (ModeKind::Follow, Target::Parameter, Source::Pedal) => {
            "Follows the pedal (range in the block ⋮ menu)"
        }
        (ModeKind::Follow, _, Source::Pedal) => "Pedal switch (settings in the block ⋮ menu)",
        (ModeKind::Follow, Target::Parameter, _) => "The CC sets the value",
        (ModeKind::Follow, ..) => "On while the CC is in its window",
        (ModeKind::Relative, ..) => "An endless encoder steps the value",
        (ModeKind::Set, _, Source::Load) => "Sets the value when the preset loads",
        (ModeKind::Set, ..) => "Each press sets the value",
        (ModeKind::Toggle, Target::Parameter, _) => "Each press swaps A and B",
        (ModeKind::Toggle, _, Source::Load) => "Switches when the preset loads",
        (ModeKind::Toggle, ..) => "Each press switches on and off",
        (ModeKind::Momentary, Target::Parameter, _) => "Held: the value; released: as before",
        (ModeKind::Momentary, Target::Looper, _) => "The button is held while you hold",
        (ModeKind::Momentary, ..) => "Switched while held",
        (ModeKind::Step, ..) => "Each press moves the value",
    };
    ModeView {
        kind,
        label: mode_label(kind).into(),
        detail: detail.into(),
    }
}

/// On and off of a switch action.
fn switch_text(action: &StoredAction, on: bool) -> &'static str {
    match (action, on) {
        (StoredAction::Block { .. }, true) => "Engaged",
        (StoredAction::Block { .. }, false) => "Bypassed",
        (_, true) => "Open",
        (_, false) => "Closed",
    }
}

fn field_label(field: FieldId) -> &'static str {
    match field {
        FieldId::Value => "Value",
        FieldId::A => "A",
        FieldId::B => "B",
        FieldId::Amount => "Amount",
        FieldId::Wrap => "At the ends",
        FieldId::Min => "Min",
        FieldId::Max => "Max",
        FieldId::Pickup => "Pickup",
        FieldId::Encoding => "Encoding",
        FieldId::Low => "Low",
        FieldId::High => "High",
    }
}

/// The parts of a row that come from its mode.
struct ModeParts {
    mode: ModeView,
    modes: Vec<ModeView>,
    fields: Vec<ValueField>,
    summary: String,
    response: Option<ResponseView>,
}

/// A normalized value in thousandths, for the Values page graph.
fn thousandths(value: f32) -> u16 {
    (value.clamp(0.0, 1.0) * 1000.0).round() as u16
}

impl Mappings {
    /// A row of the page; `key` names a Tetrad row (stock rows have no runtime state).
    fn row(
        &self,
        state: &State,
        midi: &PresetMidiView,
        extension_triggers: &[ExtensionTriggerChoice],
        id: MappingId,
        stored: &Stored,
        key: Option<RowKey>,
    ) -> MappingRow {
        let trigger = &stored.trigger;
        let action = stored.action.as_ref();
        let (trigger_label, trigger_detail) = trigger_text(trigger, extension_triggers);
        let (action_label, action_detail) = action.map_or_else(Default::default, |action| {
            action_text(state, midi, trigger, action)
        });
        let parts = action.map(|action| self.mode_parts(state, stored, action, key));
        let (mode, modes, fields, summary, response) = match parts {
            Some(parts) => (
                Some(parts.mode),
                parts.modes,
                parts.fields,
                parts.summary,
                parts.response,
            ),
            None => Default::default(),
        };
        MappingRow {
            id,
            trigger: ui_trigger(trigger),
            action: action.and_then(ui_action),
            trigger_label,
            trigger_detail,
            action_label,
            action_detail,
            mode,
            modes,
            fields,
            summary,
            response,
        }
    }

    fn mode_parts(
        &self,
        state: &State,
        stored: &Stored,
        action: &StoredAction,
        key: Option<RowKey>,
    ) -> ModeParts {
        let (source, target) = (source(&stored.trigger), target(action));
        let mode = effective_mode(stored);
        let window = stored.window.unwrap_or(mode.default_window(target));
        let param = action_param(state, action);
        let modes = mode::modes(source, target);
        let value_text = |value: f32| match &param {
            Some(param) => param.text(value),
            None if target == Target::Switch => switch_text(action, value >= 0.5).into(),
            // The block was moved or replaced.
            None => "-".into(),
        };
        let ids = mode::fields(mode, source, target);
        let fields = (ids.iter())
            .map(|&id| {
                let text = match (id, mode) {
                    (FieldId::Value, Mode::Set { value } | Mode::Momentary { value })
                    | (FieldId::A, Mode::Toggle { a: value, .. })
                    | (FieldId::B, Mode::Toggle { b: value, .. })
                    | (FieldId::Min, Mode::Follow { min: value, .. })
                    | (FieldId::Max, Mode::Follow { max: value, .. }) => value_text(value),
                    (FieldId::Amount, Mode::Step { amount, .. }) => format!("{amount:+}"),
                    (FieldId::Wrap, Mode::Step { wrap, .. }) => {
                        if wrap { "Wrap" } else { "Stop" }.into()
                    }
                    (FieldId::Pickup, Mode::Follow { pickup, .. }) => {
                        if pickup { "Pickup" } else { "Jump" }.into()
                    }
                    (FieldId::Encoding, Mode::Relative { encoding }) => {
                        mode::encoding_text(encoding).into()
                    }
                    (FieldId::Low, _) => format!("CC {}", window.low),
                    (FieldId::High, _) => format!("CC {}", window.high),
                    _ => String::new(),
                };
                ValueField {
                    id,
                    label: field_label(id).into(),
                    text,
                }
            })
            .collect();
        let values = match (target, mode) {
            (Target::Parameter, Mode::Set { value }) | (Target::Switch, Mode::Set { value }) => {
                value_text(value)
            }
            (Target::Parameter, Mode::Toggle { a, b }) => {
                format!("{} ↔ {}", value_text(a), value_text(b))
            }
            (Target::Parameter, Mode::Momentary { value }) => {
                format!("{} while held", value_text(value))
            }
            (Target::Parameter, Mode::Step { amount, wrap }) => {
                let ends = if wrap { " · wrap" } else { "" };
                format!("{amount:+} per press{ends}")
            }
            (Target::Parameter, Mode::Follow { min, max, pickup }) => {
                let pickup = if pickup { " · pickup" } else { "" };
                format!("{} – {}{pickup}", value_text(min), value_text(max))
            }
            (Target::Parameter, Mode::Relative { encoding }) => {
                mode::encoding_text(encoding).into()
            }
            _ => String::new(),
        };
        let windowed = ids.contains(&FieldId::Low);
        let window_text = windowed.then(|| format!("CC {}–{}", window.low, window.high));
        let summary = [Some(values).filter(|v| !v.is_empty()), window_text]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let follow = match (mode, &param) {
            (Mode::Follow { min, max, pickup }, Some(param)) => Some((min, max, pickup, param)),
            _ => None,
        };
        let response = windowed.then(|| ResponseView {
            low: window.low,
            high: window.high,
            line: follow.map(|(min, max, ..)| (thousandths(min), thousandths(max))),
            level: self.levels.get(&level_key(&stored.trigger)).copied(),
            current: follow.map(|(.., param)| thousandths(param.current)),
            waiting: follow.is_some_and(|(_, _, pickup, param)| {
                pickup && key.is_none_or(|key| !self.caught(key, param))
            }),
        });
        ModeParts {
            mode: mode_view(mode.kind(), source, target),
            modes: if modes.len() > 1 {
                (modes.iter())
                    .map(|&kind| mode_view(kind, source, target))
                    .collect()
            } else {
                Vec::new()
            },
            fields,
            summary,
            response,
        }
    }
}

/// The stock rows of the loaded preset with their triggers (for ordering).
fn stock_rows(
    mappings: &Mappings,
    state: &State,
    midi: &PresetMidiView,
) -> Vec<(StoredTrigger, MappingRow)> {
    let Some(current) = state.current.as_ref() else {
        return Vec::new();
    };
    let preset = &current.preset;
    let mut rows = Vec::new();
    let mut push = |id: MappingId, trigger: StoredTrigger, action: StoredAction| {
        let mut stored = Stored::new(trigger, Some(action));
        if let StoredTrigger::Stomp { stomp } = stored.trigger {
            stored.mode = Some(stomp_mode(state, stomp));
        }
        let row = mappings.row(state, midi, &[], id, &stored, None);
        rows.push((stored.trigger, row));
    };
    let block = |position: GridPosition, target: StoredTarget| StoredAction::Block {
        row: position.row(),
        column: position.column(),
        model: model_at(state, position).unwrap_or_default(),
        target,
    };
    for assignment in &preset.stomp_mode_assignments {
        // A secondary assignment drives the block's secondary stomp parameter, not its
        // bypass; Mappings leaves those to the grid.
        if !matches!(assignment.role, None | Some(StompRole::Primary)) {
            continue;
        }
        let (Ok(row), Ok(column), Ok(stomp)) = (
            u8::try_from(assignment.row),
            u8::try_from(assignment.column),
            u8::try_from(assignment.stomp_index),
        ) else {
            continue;
        };
        let Some(position) = position(row, column) else {
            continue;
        };
        push(
            MappingId::Stomp { stomp, position },
            StoredTrigger::Stomp { stomp },
            block(position, StoredTarget::Bypass),
        );
    }
    for row in 0..tetrad_engine::ROWS as u8 {
        for column in 0..tetrad_engine::COLUMNS as u8 {
            let slot = tetrad_preset::Slot {
                row: usize::from(row),
                column: usize::from(column),
            };
            let (Ok(Some(model)), Some(position)) = (preset.block(slot), position(row, column))
            else {
                continue;
            };
            let links = (model.params.iter().enumerate())
                .filter_map(|(index, param)| {
                    Some((
                        param.expression?,
                        StoredTarget::Parameter {
                            index: index as u32,
                        },
                    ))
                })
                .chain(
                    model
                        .bypass_expression
                        .first()
                        .map(|bypass| (bypass.expression, StoredTarget::Bypass)),
                );
            for (code, target) in links {
                let Some(pedal) = Pedal::from_assignment(code) else {
                    continue;
                };
                let pedal = pedal as u8;
                push(
                    MappingId::Expression {
                        pedal,
                        position,
                        target: ui_target(target),
                    },
                    StoredTrigger::Expression { pedal },
                    block(position, target),
                );
            }
        }
    }
    let targets = std::iter::once(MidiOutTarget::OnLoad)
        .chain(
            Footswitch::TILES
                .iter()
                .map(|switch| MidiOutTarget::Footswitch(*switch)),
        )
        .chain([MidiOutTarget::Expression1, MidiOutTarget::Expression2]);
    for target in targets {
        if !midi.messages(target).is_empty() {
            push(
                MappingId::MidiOut(target),
                midi_out_trigger(target),
                StoredAction::SendMidi,
            );
        }
    }
    rows
}

fn blocks(state: &State) -> Vec<MappingBlock> {
    let (Some(current), Some(specs)) = (state.current.as_ref(), state.specs.as_ref()) else {
        return Vec::new();
    };
    let layout = current.deployment.layout();
    let mut blocks = Vec::new();
    for row in 0..tetrad_engine::ROWS as u8 {
        for column in 0..tetrad_engine::COLUMNS as u8 {
            let Some(position) = position(row, column) else {
                continue;
            };
            let Some(instance) =
                engine_slot(position).and_then(|slot| layout.instance(Site::Slot(slot)))
            else {
                continue;
            };
            let spec = specs.model(instance.model.0);
            let parameters = (0..instance.params.len() as u32)
                .filter_map(|index| {
                    let parameter = spec?.parameter(index)?;
                    (parameter.kind != tetrad_catalog::ParameterType::String
                        && !parameter.scale.disabled)
                        .then(|| MappingParameter {
                            index,
                            name: parameter.name.clone(),
                        })
                })
                .collect();
            blocks.push(MappingBlock {
                position,
                name: spec.map_or_else(
                    || format!("Model {}", instance.model.0),
                    |spec| spec.name.clone(),
                ),
                location: format!("Row {} slot {}", row + 1, column + 1),
                parameters,
            });
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows that the page could not make are dropped when a list is read: a preset can come
    /// from anyone.
    #[test]
    fn decode_drops_rows_the_page_could_not_make() {
        let json = br#"{"format":1,"mappings":[
            {"trigger":{"kind":"cc","controller":20},"action":{"kind":"system","action":"tuner"}},
            {"trigger":{"kind":"cc","controller":43},"action":{"kind":"system","action":"tuner"}},
            {"trigger":{"kind":"presetLoaded"},"action":{"kind":"system","action":"nextPreset"}},
            {"trigger":{"kind":"stomp","stomp":0},"action":{"kind":"block","row":0,"column":2,
                "model":1,"target":{"kind":"parameter","index":0}},"value":255},
            {"trigger":{"kind":"stomp","stomp":1},"action":{"kind":"block","row":0,"column":2,
                "model":1,"target":{"kind":"bypass"}}}
        ]}"#;
        let preset = decode(json, MappingScope::Preset).unwrap();
        let triggers: Vec<_> = preset.iter().map(|stored| stored.trigger.clone()).collect();
        assert_eq!(
            triggers,
            [
                StoredTrigger::Cc { controller: 20 },
                StoredTrigger::Stomp { stomp: 1 }
            ]
        );
        // Global lists take only MIDI triggers and no block actions.
        let global = decode(json, MappingScope::Global).unwrap();
        assert_eq!(global.len(), 1);
    }

    /// A percent from before modes reads as Set; modes and windows that do not fit the row are
    /// dropped with it.
    #[test]
    fn decode_reads_old_values_and_checks_modes() {
        let block = r#""action":{"kind":"block","row":0,"column":2,"model":1,
            "target":{"kind":"parameter","index":0}}"#;
        let json = format!(
            r#"{{"format":1,"mappings":[
            {{"trigger":{{"kind":"stomp","stomp":0}},{block},"value":60}},
            {{"trigger":{{"kind":"note","key":1}},{block},"mode":{{"kind":"relative","encoding":"offset"}}}},
            {{"trigger":{{"kind":"note","key":2}},{block},"window":{{"low":0,"high":10}}}},
            {{"trigger":{{"kind":"cc","controller":20}},{block},"window":{{"low":90,"high":10}}}},
            {{"trigger":{{"kind":"cc","controller":21}},{block},"mode":{{"kind":"set","value":1.5}}}},
            {{"trigger":{{"kind":"cc","controller":22}},{block},
                "mode":{{"kind":"follow","min":1,"max":0,"pickup":true}},"window":{{"low":10,"high":20}}}}
        ]}}"#
        );
        let rows = decode(json.as_bytes(), MappingScope::Preset).unwrap();
        let modes: Vec<_> = rows
            .iter()
            .map(|stored| (stored.mode, stored.window))
            .collect();
        assert_eq!(
            modes,
            [
                (Some(Mode::Set { value: 0.6 }), None),
                (
                    Some(Mode::Follow {
                        min: 1.0,
                        max: 0.0,
                        pickup: true
                    }),
                    Some(Window { low: 10, high: 20 })
                ),
            ]
        );
        // The old percent is never written again.
        let written = String::from_utf8(encode(&rows)).unwrap();
        assert!(!written.contains("\"value\":60"), "{written}");
    }
}
