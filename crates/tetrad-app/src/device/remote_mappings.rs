//! Remote (owned, Tetrad; docs/remote.md): the Mappings page: the mappings of the loaded preset
//! and the global mappings, edited as on the unit (docs/mappings.md). A command takes the path
//! of a touch on the unit's Mappings page ([`Handoff::Mappings`]), so both pages stay in step
//! and the bridge's own refusals apply.
//!
//! (O) A row id and an action key are the `Debug` text of the `tetrad_ui` value. The page sends
//! them back as it got them; the device finds them in the view again, so they are valid while
//! the build runs and are never stored.

use serde::Deserialize;
use serde_json::{Value, json};
use tetrad_remote::{EditorOp, Reply};
use tetrad_ui::{
    BlockTarget, CUSTOM_SLOTS, CustomModeView, FieldId, LooperButton, MappingAction,
    MappingCommand, MappingRow, MappingScope, MappingsView, MidiInput, ModeChoice, ModeKind,
    PageStep, SystemAction, Trigger,
};

use super::platform::Platform;
use super::ui::Handoff;
use super::{Device, DeviceError, Subsystem, custom, mappings};

/// One command of the page: `{"op": "mode", "scope": "global", "id": "Tetrad(0)", "mode":
/// "toggle"}`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Command {
    Add {
        scope: Scope,
    },
    Trigger {
        scope: Scope,
        id: String,
        trigger: TriggerInput,
    },
    /// `action`: a key of the view's action menu.
    Action {
        scope: Scope,
        id: String,
        action: String,
    },
    /// `mode`: a kind of the row's `modes`.
    Mode {
        scope: Scope,
        id: String,
        mode: String,
    },
    /// A stepper tap on one of the row's `fields`: `ticks` steps of the value's own size.
    Step {
        scope: Scope,
        id: String,
        field: String,
        ticks: i32,
    },
    /// Start or cancel LEARN on a MIDI or extension trigger.
    Learn {
        scope: Scope,
        id: String,
        on: bool,
    },
    Remove {
        scope: Scope,
        id: String,
    },
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Scope {
    Preset,
    Global,
}

impl From<Scope> for MappingScope {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::Preset => Self::Preset,
            Scope::Global => Self::Global,
        }
    }
}

/// A trigger as the row's `trigger` shows it; an extension trigger without its source.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum TriggerInput {
    PresetLoaded,
    Stomp { stomp: u8 },
    Expression { pedal: u8 },
    Cc { number: u8 },
    Note { number: u8 },
    Extension { key: String, trigger: String },
}

/// One group of the action menu: SYSTEM, the Custom mode, then each block and MIDI OUT.
struct Group {
    label: String,
    /// Blocks and Send MIDI exist in a preset only.
    preset_only: bool,
    actions: Vec<(String, MappingAction)>,
}

impl<P: Platform> Device<P> {
    /// The view, also as the answer to a command: the page redraws from one reply.
    pub(super) fn remote_mappings(&mut self, op: EditorOp) -> Reply {
        let view = self.remote_mappings_view();
        let menu = action_menu(&view, custom::view(&self.state).as_ref());
        let EditorOp::Command { json } = op else {
            return Reply::Json(viewed(&view, &menu));
        };
        let command = serde_json::from_slice::<Command>(&json)
            .map_err(|error| format!("Not a command: {error}"))
            .and_then(|command| checked(&view, &menu, command));
        let command = match command {
            Ok(Some(command)) => command,
            // Nothing to change, as a touch on the value that is already set.
            Ok(None) => return Reply::Json(viewed(&view, &menu)),
            Err(reason) => return Reply::Refused(reason),
        };
        let applied = format!("Remote: mappings: {command:?}");
        use tetrad_dsp_host::session::Clock;
        let now = self.clock.now();
        match self.handoff(Handoff::Mappings(command), now) {
            Ok(effects) => self.apply(Ok::<_, DeviceError>(effects)),
            Err(error) => return Reply::Refused(error.to_string()),
        }
        let after = self.remote_mappings_view();
        // The bridge refuses with a notice and no change, such as a CC of the stock MIDI map.
        // The unit's page shows the same notice.
        if let Some(notice) = &after.notice
            && after.preset == view.preset
            && after.global == view.global
        {
            return Reply::Refused(notice.clone());
        }
        self.notice(Subsystem::Remote, applied);
        let menu = action_menu(&after, custom::view(&self.state).as_ref());
        Reply::Json(viewed(&after, &menu))
    }

    /// The view as the unit's page gets it at the end of this pass.
    fn remote_mappings_view(&mut self) -> MappingsView {
        let triggers = self.bridges.extensions.declared_triggers();
        let bridge = &mut self.bridges.mappings;
        bridge.sync(&self.state);
        bridge.view(&self.state, triggers)
    }
}

/// The touch command for `command` when the view offers it, `None` when it changes nothing.
/// The bridge checks the rest (the stock MIDI map, which trigger takes which action) and
/// refuses with a notice.
fn checked(
    view: &MappingsView,
    menu: &[Group],
    command: Command,
) -> Result<Option<MappingCommand>, String> {
    let find = |scope: Scope, id: &str| {
        let scope = MappingScope::from(scope);
        (view.rows(scope).iter())
            .find(|row| format!("{:?}", row.id) == id)
            .map(|row| (scope, row))
            .ok_or_else(|| format!("No mapping {id} in this list now; reload the page"))
    };
    Ok(Some(match command {
        Command::Add { scope } => MappingCommand::Add(scope.into()),
        Command::Trigger { scope, id, trigger } => {
            let (scope, row) = find(scope, &id)?;
            let trigger = match trigger {
                TriggerInput::PresetLoaded => Trigger::PresetLoaded,
                TriggerInput::Stomp { stomp } => Trigger::Stomp(stomp),
                TriggerInput::Expression { pedal } => Trigger::Expression(pedal),
                TriggerInput::Cc { number } => Trigger::Midi(MidiInput::Cc(number)),
                TriggerInput::Note { number } => Trigger::Midi(MidiInput::Note(number)),
                TriggerInput::Extension { key, trigger } => {
                    let declared = (view.extension_triggers.iter())
                        .any(|choice| choice.key == key && choice.trigger == trigger);
                    if !declared {
                        return Err(format!("{key} has no trigger {trigger} now"));
                    }
                    // The same trigger keeps its learned source; another one waits for LEARN.
                    let source = match &row.trigger {
                        Trigger::Extension {
                            key: was,
                            trigger: was_trigger,
                            source,
                        } if *was == key && *was_trigger == trigger => source.clone(),
                        _ => String::new(),
                    };
                    Trigger::Extension {
                        key,
                        trigger,
                        source,
                    }
                }
            };
            if trigger == row.trigger {
                return Ok(None);
            }
            MappingCommand::SetTrigger {
                scope,
                id: row.id.clone(),
                trigger,
            }
        }
        Command::Action { scope, id, action } => {
            let (scope, row) = find(scope, &id)?;
            let action = (menu.iter())
                .filter(|group| scope == MappingScope::Preset || !group.preset_only)
                .flat_map(|group| &group.actions)
                .map(|(_, offered)| offered)
                .find(|offered| format!("{offered:?}") == action)
                .ok_or_else(|| format!("{action} is not an action of this list"))?;
            if row.action.as_ref() == Some(action) {
                return Ok(None);
            }
            MappingCommand::SetAction {
                scope,
                id: row.id.clone(),
                action: action.clone(),
            }
        }
        Command::Mode { scope, id, mode } => {
            let (scope, row) = find(scope, &id)?;
            let kind = (row.modes.iter())
                .map(|offered| offered.kind)
                .find(|&kind| mode_key(kind) == mode)
                .ok_or_else(|| format!("{} cannot use the mode {mode}", row.trigger_label))?;
            if row.mode.as_ref().map(|mode| mode.kind) == Some(kind) {
                return Ok(None);
            }
            MappingCommand::SetMode {
                scope,
                id: row.id.clone(),
                mode: kind,
            }
        }
        Command::Step {
            scope,
            id,
            field,
            ticks,
        } => {
            let (scope, row) = find(scope, &id)?;
            let shown = (row.fields.iter())
                .map(|shown| shown.id)
                .find(|&shown| field_key(shown) == field)
                .ok_or_else(|| format!("{} has no value {field}", row.trigger_label))?;
            if ticks == 0 || ticks.abs() > 127 {
                return Err("ticks goes from -127 to 127 and is not 0".into());
            }
            MappingCommand::StepField {
                scope,
                id: row.id.clone(),
                field: shown,
                ticks,
            }
        }
        Command::Learn { scope, id, on } => {
            let (scope, row) = find(scope, &id)?;
            if on && !learns(&row.trigger) {
                return Err("Only MIDI and extension triggers learn".into());
            }
            MappingCommand::Learn {
                scope,
                id: row.id.clone(),
                on,
            }
        }
        Command::Remove { scope, id } => {
            let (scope, row) = find(scope, &id)?;
            MappingCommand::Remove {
                scope,
                id: row.id.clone(),
            }
        }
    }))
}

fn learns(trigger: &Trigger) -> bool {
    matches!(trigger, Trigger::Midi(_) | Trigger::Extension { .. })
}

/// The action menu of the unit's Mappings page ([`tetrad_ui`] `screens::mappings::entries`),
/// without the slot-only actions of the Custom mode editor.
fn action_menu(view: &MappingsView, custom: Option<&CustomModeView>) -> Vec<Group> {
    use SystemAction as S;
    let named = |actions: Vec<SystemAction>| {
        (actions.into_iter())
            .map(|action| {
                let (label, detail) = mappings::system_text(action);
                let label = match action {
                    S::Tuner | S::GigView => label,
                    _ => detail,
                };
                (label, MappingAction::System(action))
            })
            .collect()
    };
    let mut system = vec![S::NextPreset, S::PreviousPreset];
    system.extend((0..8).map(S::Scene));
    system.extend([S::NextScene, S::PreviousScene, S::Tuner, S::TapTempo]);
    system.extend([ModeChoice::Preset, ModeChoice::Scene, ModeChoice::Stomp].map(S::Mode));
    system.extend(custom.map(|_| S::Mode(ModeChoice::Custom)));
    system.extend([S::NextMode, S::GigView]);
    system.extend(
        [
            LooperButton::RecordOverdub,
            LooperButton::PlayStop,
            LooperButton::Undo,
            LooperButton::Reverse,
            LooperButton::HalfSpeed,
            LooperButton::OneShot,
            LooperButton::PunchIn,
            LooperButton::Duplicate,
        ]
        .map(S::Looper),
    );
    let mut groups = vec![Group {
        label: "System".into(),
        preset_only: false,
        actions: named(system),
    }];
    if let Some(custom) = custom {
        let mut actions = vec![
            S::CustomPage(PageStep::Next),
            S::CustomPage(PageStep::Previous),
        ];
        actions.extend((0..custom.pages).map(|page| S::CustomPage(PageStep::Go(page))));
        actions.extend((0..CUSTOM_SLOTS as u8).map(S::CustomSlot));
        groups.push(Group {
            label: custom.name.clone(),
            preset_only: false,
            actions: named(actions),
        });
    }
    for block in &view.blocks {
        let position = block.position;
        let action =
            |label: &str, target| (label.to_owned(), MappingAction::Block { position, target });
        let parameters = (block.parameters.iter())
            .map(|parameter| action(&parameter.name, BlockTarget::Parameter(parameter.index)));
        groups.push(Group {
            label: format!("{} · {}", block.name, block.location),
            preset_only: true,
            actions: std::iter::once(action("Bypass", BlockTarget::Bypass))
                .chain(parameters)
                .collect(),
        });
    }
    groups.push(Group {
        label: "MIDI out".into(),
        preset_only: true,
        actions: vec![("Send MIDI".into(), MappingAction::SendMidi)],
    });
    groups
}

fn mode_key(kind: ModeKind) -> &'static str {
    match kind {
        ModeKind::OnPress => "onPress",
        ModeKind::Follow => "follow",
        ModeKind::Relative => "relative",
        ModeKind::Set => "set",
        ModeKind::Toggle => "toggle",
        ModeKind::Momentary => "momentary",
        ModeKind::Step => "step",
    }
}

fn field_key(field: FieldId) -> &'static str {
    match field {
        FieldId::Value => "value",
        FieldId::A => "a",
        FieldId::B => "b",
        FieldId::Amount => "amount",
        FieldId::Wrap => "wrap",
        FieldId::Min => "min",
        FieldId::Max => "max",
        FieldId::Pickup => "pickup",
        FieldId::Encoding => "encoding",
        FieldId::Low => "low",
        FieldId::High => "high",
    }
}

fn scope_key(scope: MappingScope) -> &'static str {
    match scope {
        MappingScope::Preset => "preset",
        MappingScope::Global => "global",
    }
}

/// The view as the page reads it (docs/remote.md).
fn viewed(view: &MappingsView, menu: &[Group]) -> Value {
    let spot = |(scope, id): &(MappingScope, tetrad_ui::MappingId)| json!({ "scope": scope_key(*scope), "id": format!("{id:?}") });
    let per_scope = |list: &dyn Fn(MappingScope) -> Value| json!({ "preset": list(MappingScope::Preset), "global": list(MappingScope::Global) });
    json!({
        "preset": view.preset_name,
        "rows": per_scope(&|scope| view.rows(scope).iter().map(row).collect()),
        "triggers": per_scope(&|scope| triggers(view, scope)),
        "actions": per_scope(&|scope| actions(menu, scope)),
        "focus": view.focus.as_ref().map(spot),
        "learning": view.learning.as_ref().map(spot),
        "notice": view.notice,
    })
}

/// The Type list of a scope, as on the unit: Preset loaded, footswitches and pedals only in a
/// preset, then MIDI on the QC channel and the extensions' triggers.
fn triggers(view: &MappingsView, scope: MappingScope) -> Value {
    let own: &[(&str, &str)] = match scope {
        MappingScope::Preset => &[
            ("presetLoaded", "Preset loaded"),
            ("stomp", "Footswitch"),
            ("expression", "Expression pedal"),
            ("midi", "MIDI (QC channel)"),
        ],
        MappingScope::Global => &[("midi", "MIDI (QC channel)")],
    };
    let own = (own.iter()).map(|(kind, label)| json!({ "kind": kind, "label": label }));
    let extensions = (view.extension_triggers.iter()).map(|choice| {
        json!({
            "kind": "extension", "key": choice.key, "trigger": choice.trigger,
            "label": choice.label,
        })
    });
    own.chain(extensions).collect()
}

fn actions(menu: &[Group], scope: MappingScope) -> Value {
    (menu.iter())
        .filter(|group| scope == MappingScope::Preset || !group.preset_only)
        .map(|group| {
            let actions: Vec<_> = (group.actions.iter())
                .map(|(label, action)| json!({ "key": format!("{action:?}"), "label": label }))
                .collect();
            json!({ "label": group.label, "actions": actions })
        })
        .collect()
}

fn row(row: &MappingRow) -> Value {
    let trigger = match &row.trigger {
        Trigger::PresetLoaded => json!({ "kind": "presetLoaded" }),
        Trigger::Stomp(stomp) => json!({ "kind": "stomp", "stomp": stomp }),
        Trigger::Expression(pedal) => json!({ "kind": "expression", "pedal": pedal }),
        Trigger::Midi(MidiInput::Cc(number)) => json!({ "kind": "cc", "number": number }),
        Trigger::Midi(MidiInput::Note(number)) => json!({ "kind": "note", "number": number }),
        Trigger::Extension {
            key,
            trigger,
            source,
        } => json!({ "kind": "extension", "key": key, "trigger": trigger, "source": source }),
    };
    let mode = |mode: &tetrad_ui::ModeView| json!({ "kind": mode_key(mode.kind), "label": mode.label, "detail": mode.detail });
    let fields: Vec<_> = (row.fields.iter())
        .map(|field| json!({ "id": field_key(field.id), "label": field.label, "text": field.text }))
        .collect();
    let response = row.response.as_ref().map(|response| {
        json!({
            "low": response.low,
            "high": response.high,
            "line": response.line.map(|(low, high)| [low, high]),
            "level": response.level,
            "current": response.current,
            "waiting": response.waiting,
        })
    });
    json!({
        "id": format!("{:?}", row.id),
        "trigger": trigger,
        "triggerLabel": row.trigger_label,
        "triggerDetail": row.trigger_detail,
        "learns": learns(&row.trigger),
        "action": row.action.as_ref().map(|action| format!("{action:?}")),
        "actionLabel": row.action_label,
        "actionDetail": row.action_detail,
        "sendMidi": row.action == Some(MappingAction::SendMidi),
        "mode": row.mode.as_ref().map(mode),
        "modes": row.modes.iter().map(mode).collect::<Vec<_>>(),
        "fields": fields,
        "summary": row.summary,
        "response": response,
    })
}
