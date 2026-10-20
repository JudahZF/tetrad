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

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tetrad_core::performance::expression::Pedal;
use tetrad_engine::{Column, Row, SceneIndex, Site, Slot};
use tetrad_midi::MidiMessage;
use tetrad_preset::StompRole;
use tetrad_preset::model::TETRAD_MAPPINGS_FIELD;
use tetrad_settings::controls::FootswitchMode;
use tetrad_ui::{
    BlockTarget, ExtensionTriggerChoice, Footswitch, GridPosition, LooperButton, MappingAction,
    MappingBlock, MappingCommand, MappingId, MappingParameter, MappingRow, MappingScope,
    MappingsView, MidiInput, MidiMessageKind, MidiOutMessage, MidiOutTarget, ModeChoice,
    PresetMidiView, Screen, SystemAction, Trigger,
};

use super::extensions::FiredTrigger;
use super::preset::PresetEdit;
use super::{Action, Effect, PresetRef, State};

/// The device's global mappings.
pub const GLOBAL_FILE: &str = "/media/p4/tetrad/mappings.json";
/// Stored mappings per list; the file and the preset field are refused above it.
const MAX_MAPPINGS: usize = 128;
const FORMAT: u32 = 1;

/// One Tetrad mapping as stored (JSON, [`StoredList`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    trigger: StoredTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action: Option<StoredAction>,
    /// The one-shot value of a parameter action, in percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

fn decode(bytes: &[u8]) -> Result<Vec<Stored>, String> {
    let list: StoredList = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if list.format != FORMAT {
        return Err(format!("format {} is not supported", list.format));
    }
    if list.mappings.len() > MAX_MAPPINGS {
        return Err(format!("more than {MAX_MAPPINGS} mappings"));
    }
    Ok(list.mappings)
}

/// How a trigger reports.
#[derive(Debug, Clone, Copy)]
struct Input {
    /// 0..=1.
    value: f32,
    /// A CC or an extension's continuous source; otherwise a press.
    continuous: bool,
}

const PRESS: Input = Input {
    value: 1.0,
    continuous: false,
};

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
        match bytes.map(decode) {
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
            .and_then(|bytes| decode(&bytes).ok())
            .unwrap_or_default();
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
                Input {
                    value: f32::from(value.get()) / 127.0,
                    continuous: true,
                },
            ),
            MidiMessage::NoteOn { key, velocity, .. } if velocity.get() > 0 => (
                StoredTrigger::Note { key: key.get() },
                Input {
                    value: f32::from(velocity.get()) / 127.0,
                    continuous: false,
                },
            ),
            _ => return Step::default(),
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
        let input = Input {
            value: fired.value.clamp(0.0, 1.0),
            continuous: fired.continuous,
        };
        self.fire(trigger, input, state, now)
    }

    /// A Stomp-mode footswitch press, 0..=7. Stock assignments toggle their blocks as before.
    pub fn on_stomp(&mut self, stomp: u8, state: &State, now: Duration) -> Vec<Effect> {
        self.sync(state);
        self.run_matching(
            MappingScope::Preset,
            &StoredTrigger::Stomp { stomp },
            PRESS,
            state,
            now,
        )
    }

    /// The preset finished loading.
    pub fn on_preset_loaded(&mut self, state: &State, now: Duration) -> Vec<Effect> {
        self.sync(state);
        self.view_stale = true;
        self.learning = None;
        self.run_matching(
            MappingScope::Preset,
            &StoredTrigger::PresetLoaded,
            PRESS,
            state,
            now,
        )
    }

    /// LEARN takes the message (a native or an extension source becomes the row's trigger);
    /// else the preset's mappings for it run, or the global ones when the preset has none.
    fn fire(&mut self, trigger: StoredTrigger, input: Input, state: &State, now: Duration) -> Step {
        if let Some((scope, id)) = self.learning.take() {
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

    fn matching(&self, scope: MappingScope, trigger: &StoredTrigger) -> Vec<Stored> {
        let same = |stored: &StoredTrigger| match (stored, trigger) {
            (
                StoredTrigger::Extension {
                    key,
                    trigger,
                    source,
                    ..
                },
                StoredTrigger::Extension {
                    key: k,
                    trigger: t,
                    source: s,
                    ..
                },
            ) => key == k && trigger == t && source == s,
            (a, b) => a == b,
        };
        (self.list(scope).iter())
            .filter(|stored| same(&stored.trigger))
            .cloned()
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
            .flat_map(|stored| self.run(stored, input, state, now))
            .collect()
    }

    // ---- actions --------------------------------------------------------------------------

    fn run(&mut self, stored: &Stored, input: Input, state: &State, now: Duration) -> Vec<Effect> {
        match &stored.action {
            Some(StoredAction::System { action }) => (self.system(*action, input, state, now))
                .into_iter()
                .map(Effect::Action)
                .collect(),
            Some(StoredAction::Block {
                row,
                column,
                model,
                target,
            }) => block_action(state, *row, *column, *model, *target, stored.value, input)
                .map(Effect::ExtensionAction)
                .into_iter()
                .collect(),
            // The stock Preset MIDI Out runtime sends the lists.
            Some(StoredAction::SendMidi) | None => Vec::new(),
        }
    }

    fn system(
        &mut self,
        action: SystemAction,
        input: Input,
        state: &State,
        now: Duration,
    ) -> Vec<Action> {
        // One-shot actions run on a press, or on a CC at 64 or more.
        let on = !input.continuous || input.value >= 0.5;
        let shown = |screen: Screen| {
            if input.continuous {
                input.value >= 0.5
            } else {
                state.screen != screen
            }
        };
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
            SystemAction::Tuner => vec![Action::ShowTuner(shown(Screen::Tuner))],
            SystemAction::GigView => vec![Action::ShowGigView(shown(Screen::Gig))],
            SystemAction::Looper(button) => {
                let button = looper_button(button);
                if input.continuous {
                    vec![Action::Looper {
                        button,
                        pressed: input.value >= 0.5,
                    }]
                } else {
                    [true, false]
                        .map(|pressed| Action::Looper { button, pressed })
                        .into()
                }
            }
            _ if !on => Vec::new(),
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
            MappingCommand::SetValue { scope, id, value } => match id {
                MappingId::Tetrad(index) => {
                    let mut list = self.list(scope).clone();
                    match list.get_mut(index as usize) {
                        Some(stored) if stored.value.is_some() => {
                            stored.value = Some(value.min(100));
                            self.focus = Some((scope, MappingId::Tetrad(index)));
                            self.store(scope, list)
                        }
                        _ => Step::default(),
                    }
                }
                _ => Step::default(),
            },
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
        list.push(Stored {
            trigger,
            action: None,
            value: None,
        });
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
                let stored = Stored {
                    trigger: StoredTrigger::Stomp { stomp },
                    action: Some(StoredAction::Block {
                        row: position.row(),
                        column: position.column(),
                        model,
                        target: StoredTarget::Bypass,
                    }),
                    value: None,
                };
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
                let stored = Stored {
                    trigger: StoredTrigger::Expression { pedal },
                    action: Some(StoredAction::Block {
                        row: position.row(),
                        column: position.column(),
                        model,
                        target: stored_target(target),
                    }),
                    value: None,
                };
                let effects = vec![link(position, target, pedal, false)?];
                Some((stored, effects, list))
            }
            MappingId::MidiOut(target) => {
                let stored = Stored {
                    trigger: midi_out_trigger(target),
                    action: Some(StoredAction::SendMidi),
                    value: None,
                };
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
        if let Some(action) = &stored.action
            && let Err(reason) = action_allowed(scope, &stored.trigger, action)
        {
            stored.action = None;
            stored.value = None;
            self.notice = Some(format!("Choose a new action: {reason}"));
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
        let one_shot_parameter = matches!(
            action,
            StoredAction::Block {
                target: StoredTarget::Parameter { .. },
                ..
            }
        ) && !continuous(&stored.trigger);
        stored.value = one_shot_parameter.then_some(stored.value.unwrap_or(100));
        stored.action = Some(action);
        let mut step = self.place(scope, stored, list, state);
        effects.append(&mut step.effects);
        step.effects = effects;
        step
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
            ) => position(*row, *column).and_then(|position| {
                let effect = Effect::Action(Action::PresetEdit(PresetEdit::Stomp {
                    slot: engine_slot(position)?,
                    assignment: Some((u32::from(*stomp), StompRole::Primary)),
                }));
                Some((
                    effect,
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
                    effect,
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
            Some((effect, id)) if scope == MappingScope::Preset => {
                self.focus = Some((scope, id));
                let mut step = self.store(scope, list);
                step.effects.insert(0, effect);
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
        let mut preset = stock_rows(state, &midi);
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
                    let row = tetrad_row(
                        state,
                        &midi,
                        &extension_triggers,
                        MappingId::Tetrad(index as u32),
                        stored,
                    );
                    (stored.trigger.clone(), row)
                }),
        );
        preset.sort_by_key(|(trigger, _)| rank(trigger));
        let global = (self.global.iter().enumerate())
            .map(|(index, stored)| {
                tetrad_row(
                    state,
                    &midi,
                    &extension_triggers,
                    MappingId::Tetrad(index as u32),
                    stored,
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

fn continuous(trigger: &StoredTrigger) -> bool {
    matches!(
        trigger,
        StoredTrigger::Cc { .. } | StoredTrigger::Expression { .. }
    ) || matches!(trigger, StoredTrigger::Extension { source, .. } if source.starts_with("cc:"))
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
        StoredAction::System { .. } => Ok(()),
    }
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
        Some(("pc", n)) => n
            .parse::<u16>()
            .map_or_else(|_| source.to_owned(), |n| format!("PC {}", n + 1)),
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

/// A block action for one trigger value, or `None` when the block is gone or replaced.
fn block_action(
    state: &State,
    row: u8,
    column: u8,
    model: u32,
    target: StoredTarget,
    value: Option<u8>,
    input: Input,
) -> Option<Action> {
    let current = state.current.as_ref()?;
    let slot = engine_slot(position(row, column)?)?;
    let site = Site::Slot(slot);
    let instance = current.deployment.layout().instance(site)?;
    if instance.model.0 != model {
        return None;
    }
    Some(match target {
        StoredTarget::Bypass => Action::SetBypass {
            site,
            bypassed: if input.continuous {
                input.value < 0.5
            } else {
                !instance.bypass.at(current.deployment.scene())
            },
        },
        StoredTarget::Parameter { index } => Action::SetParameter {
            site,
            index,
            value: if input.continuous {
                input.value
            } else {
                f32::from(value.unwrap_or(100)) / 100.0
            },
        },
    })
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
                || format!("Parameter {}", index + 1),
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
            let detail = (extension_triggers.iter())
                .find(|choice| &choice.key == key && &choice.trigger == trigger)
                .map_or_else(
                    || format!("{key} (not running)"),
                    |choice| choice.label.clone(),
                );
            let label = if source.is_empty() {
                "Waiting for LEARN".into()
            } else {
                label.clone()
            };
            (label, detail)
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

fn value_use(trigger: &StoredTrigger, action: Option<&StoredAction>) -> String {
    let continuous = continuous(trigger);
    match action {
        None => "Choose an action".into(),
        Some(StoredAction::SendMidi) => "Sends the messages on each trigger".into(),
        Some(StoredAction::Block {
            target: StoredTarget::Parameter { .. },
            ..
        }) if matches!(trigger, StoredTrigger::Expression { .. }) => {
            "Follows the pedal (range in the block ⋮ menu)".into()
        }
        Some(StoredAction::Block {
            target: StoredTarget::Parameter { .. },
            ..
        }) if continuous => "Continuous · 0–127 → 0–100 %".into(),
        Some(StoredAction::Block {
            target: StoredTarget::Parameter { .. },
            ..
        }) => "Sets the value below".into(),
        Some(StoredAction::Block { .. }) if matches!(trigger, StoredTrigger::Expression { .. }) => {
            "Pedal switch (settings in the block ⋮ menu)".into()
        }
        Some(StoredAction::Block { .. }) if continuous => "Switch · 64 or more engages".into(),
        Some(StoredAction::Block { .. }) => "Toggles on each press".into(),
        Some(StoredAction::System {
            action: SystemAction::Tuner | SystemAction::GigView,
        }) if !continuous => "Opens and closes on each press".into(),
        Some(StoredAction::System { .. }) if continuous => "Switch · 64 or more = on".into(),
        Some(StoredAction::System { .. }) => "Runs on each press".into(),
    }
}

fn row(
    state: &State,
    midi: &PresetMidiView,
    extension_triggers: &[ExtensionTriggerChoice],
    id: MappingId,
    trigger: &StoredTrigger,
    action: Option<&StoredAction>,
    value: Option<u8>,
) -> MappingRow {
    let (trigger_label, trigger_detail) = trigger_text(trigger, extension_triggers);
    let (action_label, action_detail) = action.map_or_else(Default::default, |action| {
        action_text(state, midi, trigger, action)
    });
    MappingRow {
        id,
        trigger: ui_trigger(trigger),
        action: action.and_then(ui_action),
        trigger_label,
        trigger_detail,
        action_label,
        action_detail,
        value_use: value_use(trigger, action),
        value,
    }
}

fn tetrad_row(
    state: &State,
    midi: &PresetMidiView,
    extension_triggers: &[ExtensionTriggerChoice],
    id: MappingId,
    stored: &Stored,
) -> MappingRow {
    row(
        state,
        midi,
        extension_triggers,
        id,
        &stored.trigger,
        stored.action.as_ref(),
        stored.value,
    )
}

/// The stock rows of the loaded preset with their triggers (for ordering).
fn stock_rows(state: &State, midi: &PresetMidiView) -> Vec<(StoredTrigger, MappingRow)> {
    let Some(current) = state.current.as_ref() else {
        return Vec::new();
    };
    let preset = &current.preset;
    let mut rows = Vec::new();
    let mut push = |id: MappingId, trigger: StoredTrigger, action: StoredAction| {
        let row = row(state, midi, &[], id, &trigger, Some(&action), None);
        rows.push((trigger, row));
    };
    let block = |position: GridPosition, target: StoredTarget| StoredAction::Block {
        row: position.row(),
        column: position.column(),
        model: model_at(state, position).unwrap_or_default(),
        target,
    };
    for assignment in &preset.stomp_mode_assignments {
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
