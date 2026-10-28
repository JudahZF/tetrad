//! Remote (owned, Tetrad; docs/remote.md): the Footswitch page: the layouts of the Custom
//! footswitch mode, edited as on the unit. A command becomes the touch editor's
//! [`CustomCommand`] and takes its path ([`Handoff::Custom`]), so the unit's editor stays in
//! step and the same refusals apply. The page offers the choices of the touch action menu.
//! (O) Playing the switches (Gig View taps and page turns) stays on the unit.

use serde::Deserialize;
use serde_json::{Value, json};
use tetrad_library::stock::SlotSummary;
use tetrad_remote::{EditorOp, Reply};
use tetrad_ui::{
    BlockTarget, CUSTOM_SLOTS, CustomCommand, CustomModeView, CustomSlotView, GridPosition,
    LayoutScope, LooperButton, MappingBlock, MidiMessageKind, MidiOutMessage, ModeChoice, PageStep,
    SlotAction, SlotEdit, SystemAction,
};

use super::mappings::{self, SystemDef};
use super::platform::Platform;
use super::ui::Handoff;
use super::{Device, DeviceError, Subsystem, custom};

/// One command of the page: `{"op": "slot", "scope": "global", "page": 0, "slot": 3,
/// "edit": {"kind": "label", "label": "Lead"}}`. Pages are 0-based, slots 0..=9 (A–H, Down,
/// Tap).
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum Command {
    Slot {
        scope: Scope,
        page: u8,
        slot: u8,
        edit: Edit,
    },
    CopyPage {
        scope: Scope,
        from: u8,
        to: u8,
    },
    /// THIS PRESET gets a copy of ALL PRESETS (`true`), or loses its own layout.
    PresetLayout {
        own: bool,
    },
    /// Reads the presets of one setlist for "Choose a preset". It changes nothing.
    Presets {
        setlist: String,
    },
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Scope {
    Global,
    Preset,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Edit {
    Action {
        action: Action,
    },
    Clear,
    Label {
        label: String,
    },
    /// `null` takes the colour from the action.
    Colour {
        colour: Option<u8>,
    },
    /// Percent, for a parameter action.
    Value {
        value: u8,
    },
    /// For a bypass action.
    Momentary {
        momentary: bool,
    },
    /// For a Send MIDI action: the whole list.
    Messages {
        messages: Vec<Message>,
    },
    CopyTo {
        page: u8,
        slot: u8,
    },
    /// The two slots swap.
    MoveTo {
        page: u8,
        slot: u8,
    },
}

/// A slot action as the view shows it and a command sets it.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Action {
    System {
        #[serde(with = "SystemDef")]
        action: SystemAction,
    },
    /// No `parameter` is the bypass.
    Block {
        row: u8,
        column: u8,
        #[serde(default)]
        parameter: Option<u32>,
    },
    /// The loaded preset. In the view, any preset: the slot's label names it.
    LoadPreset,
    /// Command only: the preset in `slot` (0-based) of `setlist`.
    Preset {
        setlist: String,
        slot: usize,
    },
    SendMidi,
    Command {
        key: String,
        command: String,
    },
}

/// A message as the MIDI Out editor makes it: CC (CC#, value), CC Toggle (CC#, min, max) or
/// PC (bank CC#0, bank CC#32, program; a bank can be `null`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Message {
    kind: Kind,
    channel: u8,
    values: [Option<u8>; 3],
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Kind {
    Cc,
    CcToggle,
    Pc,
}

impl<P: Platform> Device<P> {
    /// The view, also as the answer to a command: the page shows the new state at once.
    pub(super) fn remote_footswitch(&mut self, op: EditorOp) -> Reply {
        let mut presets = None;
        if let EditorOp::Command { json } = op {
            match self.remote_footswitch_command(&json) {
                Ok(listed) => presets = listed,
                Err(reason) => return Reply::Refused(reason),
            }
        }
        custom::sync(&mut self.state);
        let mut view = match custom::view(&self.state) {
            Some(view) => self.footswitch_view(&view),
            None => json!({ "mode": null }),
        };
        if let Some(presets) = presets {
            view["presets"] = presets;
        }
        Reply::Json(view)
    }

    /// Applies one command; a read of a setlist answers its presets.
    fn remote_footswitch_command(&mut self, json: &[u8]) -> Result<Option<Value>, String> {
        use tetrad_dsp_host::session::Clock;
        let command = serde_json::from_slice::<Command>(json)
            .map_err(|error| format!("Not a command: {error}"))?;
        custom::sync(&mut self.state);
        let view = custom::view(&self.state)
            .ok_or_else(|| "No extension adds a Custom footswitch mode".to_owned())?;
        let preset = match &command {
            Command::Presets { setlist } => return self.setlist_presets(setlist).map(Some),
            Command::Slot {
                edit:
                    Edit::Action {
                        action: Action::Preset { setlist, slot },
                    },
                ..
            } => Some((setlist.clone(), *slot)),
            _ => None,
        };
        let command = checked(&view, &self.state.views.mappings.blocks, command)?;
        let note = format!("Remote: footswitch: {command:?}");
        let effects = match preset {
            None => {
                let now = self.clock.now();
                let handoff = self.handoff(Handoff::Custom(command), now);
                handoff.map_err(|error| error.to_string())?
            }
            // "Choose a preset" names a setlist that the unit may not browse: the file comes
            // from here, not from the browsed setlist. Else as `Device::custom_command`.
            Some((setlist, slot)) => {
                let chosen = self.preset_at(&setlist, slot)?;
                let step = custom::command(&mut self.state, command, &|_| Some(chosen.clone()));
                if let Some((path, bytes)) = step.save {
                    self.write_file(&path, &bytes, "Footswitch layout not saved");
                }
                self.present = true;
                step.effects
            }
        };
        // The touch path keeps a refusal for its editor to show; the page gets it as the answer.
        if let Some(reason) = custom::view(&self.state).and_then(|view| view.notice) {
            return Err(reason);
        }
        self.notice(Subsystem::Remote, note);
        self.apply(Ok::<_, DeviceError>(effects));
        Ok(None)
    }

    /// The slots of `setlist`, each with the name of its preset.
    fn setlist_slots(&self, setlist: &str) -> Result<Vec<SlotSummary>, String> {
        let library = (self.state.library.as_ref())
            .ok_or_else(|| "The preset library is not open".to_owned())?;
        (library.presets.directory_slots(setlist)).map_err(|error| error.to_string())
    }

    /// The file and title of the preset in `slot` of `setlist`, as the touch path stores them.
    fn preset_at(&self, setlist: &str, slot: usize) -> Result<(String, String), String> {
        (self.setlist_slots(setlist)?.into_iter().nth(slot))
            .and_then(|summary| Some((summary.path?, summary.name)))
            .ok_or_else(|| "That slot is empty".to_owned())
    }

    /// The filled slots of `setlist`, labelled as the touch menu labels them ("1A Clean").
    fn setlist_presets(&self, setlist: &str) -> Result<Value, String> {
        let slots: Vec<_> = (self.setlist_slots(setlist)?.into_iter())
            .filter(|summary| summary.path.is_some())
            .map(|summary| {
                let bank = summary.slot / 8 + 1;
                let letter = char::from(b'A' + (summary.slot % 8) as u8);
                json!({ "slot": summary.slot, "label": format!("{bank}{letter} {}", summary.name) })
            })
            .collect();
        Ok(json!({ "setlist": setlist, "slots": slots }))
    }

    /// The view as the page reads it (docs/remote.md).
    fn footswitch_view(&self, view: &CustomModeView) -> Value {
        let layout = |pages: &[[CustomSlotView; CUSTOM_SLOTS]]| {
            (pages.iter())
                .map(|page| page.iter().map(slot).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        };
        let blocks: Vec<_> = (self.state.views.mappings.blocks.iter())
            .map(|block| {
                let parameters: Vec<_> = (block.parameters.iter())
                    .map(|parameter| json!({ "index": parameter.index, "name": parameter.name }))
                    .collect();
                json!({
                    "row": block.position.row(),
                    "column": block.position.column(),
                    "name": block.name,
                    "location": block.location,
                    "parameters": parameters,
                })
            })
            .collect();
        let commands: Vec<_> = (view.commands.iter())
            .map(|choice| {
                json!({ "key": choice.key, "command": choice.command, "label": choice.label })
            })
            .collect();
        // Only the user setlists: a preset action finds its file in a setlist slot again.
        let setlists: Vec<_> = (self.state.library.as_ref())
            .and_then(|library| library.presets.setlists().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|setlist| setlist.name)
            .collect();
        json!({
            "mode": {
                "name": view.name,
                "extension": view.extension,
                "pages": view.pages,
                "page": view.page,
                "active": view.active,
            },
            "preset": view.preset,
            "presetLoaded": self.state.current.is_some(),
            "colours": view.colours.iter().map(|&rgb| hex(rgb)).collect::<Vec<_>>(),
            "layouts": {
                "global": layout(&view.slots),
                "preset": view.preset_slots.as_deref().map(layout),
            },
            "systems": systems(view.pages),
            "blocks": blocks,
            "commands": commands,
            "setlists": setlists,
        })
    }
}

/// The touch command for `command`, when the view offers its targets. The touch path ignores
/// a page or slot out of range; the page gets the reason instead.
fn checked(
    view: &CustomModeView,
    blocks: &[MappingBlock],
    command: Command,
) -> Result<CustomCommand, String> {
    let page_in = |page: u8| match page < view.pages {
        true => Ok(page),
        false => Err(format!("There is no page {}", u16::from(page) + 1)),
    };
    let slot_in = |slot: u8| match usize::from(slot) < CUSTOM_SLOTS {
        true => Ok(slot),
        false => Err(format!("There is no switch {}", u16::from(slot) + 1)),
    };
    Ok(match command {
        Command::Presets { .. } => return Err("A read changes nothing".into()),
        Command::PresetLayout { own } => CustomCommand::PresetLayout(own),
        Command::CopyPage { scope, from, to } => CustomCommand::CopyPage {
            scope: scope.into(),
            from: page_in(from)?,
            to: page_in(to)?,
        },
        Command::Slot {
            scope,
            page,
            slot,
            edit,
        } => {
            let scope = LayoutScope::from(scope);
            let (page, slot) = (page_in(page)?, slot_in(slot)?);
            // THIS PRESET without its own layout: the touch path refuses with its reason.
            let current = view.slot(scope, page, usize::from(slot));
            let edit = match edit {
                Edit::Action { action } => SlotEdit::SetAction(slot_action(view, blocks, action)?),
                Edit::Clear => SlotEdit::Clear,
                Edit::Label { label } => SlotEdit::SetLabel(label),
                Edit::Colour { colour } => SlotEdit::SetColour(colour),
                Edit::Value { value } => {
                    if current.is_some_and(|slot| slot.value.is_none()) {
                        return Err("Only a parameter action has a value".into());
                    }
                    if value > 100 {
                        return Err("A value is 0 to 100 %".into());
                    }
                    SlotEdit::SetValue(value)
                }
                Edit::Momentary { momentary } => {
                    if current.is_some_and(|slot| slot.momentary.is_none()) {
                        return Err("Only a bypass action is momentary".into());
                    }
                    SlotEdit::SetMomentary(momentary)
                }
                Edit::Messages { messages } => {
                    if current.is_some_and(|slot| slot.action != Some(SlotAction::SendMidi)) {
                        return Err("Choose Send MIDI first".into());
                    }
                    let messages = messages.into_iter().map(midi_message);
                    SlotEdit::SetMessages(messages.collect::<Result<_, _>>()?)
                }
                Edit::CopyTo { page, slot } => SlotEdit::CopyTo {
                    page: page_in(page)?,
                    slot: slot_in(slot)?,
                },
                Edit::MoveTo { page, slot } => SlotEdit::MoveTo {
                    page: page_in(page)?,
                    slot: slot_in(slot)?,
                },
            };
            CustomCommand::Slot {
                scope,
                page,
                slot,
                edit,
            }
        }
    })
}

/// The touch action for `action`. A preset choice is resolved before (`preset_at`); a slot
/// press and an extension command that is not running are refused by the touch path.
fn slot_action(
    view: &CustomModeView,
    blocks: &[MappingBlock],
    action: Action,
) -> Result<SlotAction, String> {
    Ok(match action {
        Action::System {
            action: SystemAction::CustomPage(PageStep::Go(page)),
        } if page >= view.pages => return Err(format!("There is no page {}", page + 1)),
        Action::System {
            action: SystemAction::Scene(scene),
        } if scene >= 8 => return Err(format!("There is no scene {}", scene + 1)),
        Action::System { action } => SlotAction::System(action),
        Action::Block {
            row,
            column,
            parameter,
        } => {
            let position = GridPosition::new(row, column).ok();
            let block = (blocks.iter())
                .find(|block| Some(block.position) == position)
                .ok_or_else(|| "There is no block there".to_owned())?;
            let target = match parameter {
                None => BlockTarget::Bypass,
                Some(index) if block.parameters.iter().any(|p| p.index == index) => {
                    BlockTarget::Parameter(index)
                }
                Some(index) => return Err(format!("{} has no parameter {index}", block.name)),
            };
            SlotAction::Block {
                position: block.position,
                target,
            }
        }
        Action::LoadPreset => SlotAction::LoadPreset,
        Action::Preset { slot, .. } => SlotAction::PresetSlot(slot as u64),
        Action::SendMidi => SlotAction::SendMidi,
        Action::Command { key, command } => SlotAction::Command { key, command },
    })
}

/// A message with the parts its kind uses, each in range.
fn midi_message(message: Message) -> Result<MidiOutMessage, String> {
    if !(1..=16).contains(&message.channel) {
        return Err("A channel is 1 to 16".into());
    }
    let [a, b, c] = message.values;
    let (kind, values) = match message.kind {
        Kind::Cc => (MidiMessageKind::Cc, [a, b, None]),
        Kind::CcToggle => (MidiMessageKind::CcToggle, [a, b, c]),
        Kind::Pc => (MidiMessageKind::Pc, [a, b, c]),
    };
    let needed = match kind {
        MidiMessageKind::Cc => [true, true, false],
        MidiMessageKind::CcToggle => [true; 3],
        // The banks are optional.
        MidiMessageKind::Pc => [false, false, true],
    };
    if values
        .iter()
        .zip(needed)
        .any(|(value, need)| need && value.is_none())
    {
        return Err("A message needs each of its values".into());
    }
    if values.iter().flatten().any(|value| *value > 127) {
        return Err("A MIDI value is 0 to 127".into());
    }
    Ok(MidiOutMessage {
        kind,
        channel: message.channel,
        values,
    })
}

impl From<Scope> for LayoutScope {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::Global => Self::Global,
            Scope::Preset => Self::Preset,
        }
    }
}

fn hex([red, green, blue]: [u8; 3]) -> String {
    format!("#{red:02x}{green:02x}{blue:02x}")
}

fn slot(slot: &CustomSlotView) -> Value {
    let messages: Vec<_> = (slot.messages.iter())
        .map(|message| {
            let kind = match message.kind {
                MidiMessageKind::Cc => "cc",
                MidiMessageKind::CcToggle => "ccToggle",
                MidiMessageKind::Pc => "pc",
            };
            json!({ "kind": kind, "channel": message.channel, "values": message.values })
        })
        .collect();
    json!({
        "action": slot.action.as_ref().map(action),
        "label": slot.label,
        "customLabel": slot.custom_label,
        "detail": slot.detail,
        "colour": hex(slot.colour),
        "swatch": slot.swatch,
        "engaged": slot.engaged,
        "available": slot.available,
        "value": slot.value,
        "momentary": slot.momentary,
        "messages": messages,
    })
}

/// [`Action`] as JSON.
fn action(action: &SlotAction) -> Value {
    match action {
        SlotAction::System(system) => json!({ "kind": "system", "action": system_json(*system) }),
        SlotAction::Block { position, target } => json!({
            "kind": "block",
            "row": position.row(),
            "column": position.column(),
            "parameter": match target {
                BlockTarget::Bypass => None,
                BlockTarget::Parameter(index) => Some(index),
            },
        }),
        SlotAction::LoadPreset | SlotAction::PresetSlot(_) => json!({ "kind": "loadPreset" }),
        SlotAction::SendMidi => json!({ "kind": "sendMidi" }),
        SlotAction::Command { key, command } => {
            json!({ "kind": "command", "key": key, "command": command })
        }
    }
}

fn system_json(action: SystemAction) -> Value {
    SystemDef::serialize(&action, serde_json::value::Serializer).unwrap_or_default()
}

/// The SYSTEM choices of the touch action menu for a slot, in its order: `group`, `label` and
/// the `action` to send.
fn systems(pages: u8) -> Vec<Value> {
    use SystemAction as S;
    let looper = [
        LooperButton::RecordOverdub,
        LooperButton::PlayStop,
        LooperButton::Undo,
        LooperButton::Reverse,
        LooperButton::HalfSpeed,
        LooperButton::OneShot,
        LooperButton::PunchIn,
        LooperButton::Duplicate,
    ];
    let modes = [
        ModeChoice::Preset,
        ModeChoice::Scene,
        ModeChoice::Stomp,
        ModeChoice::Custom,
    ];
    let groups: [(&str, Vec<SystemAction>); 8] = [
        ("Preset", vec![S::NextPreset, S::PreviousPreset]),
        (
            "Scene",
            (0..8)
                .map(S::Scene)
                .chain([S::NextScene, S::PreviousScene])
                .collect(),
        ),
        ("Tuner", vec![S::Tuner]),
        ("Tempo", vec![S::TapTempo]),
        (
            "Mode",
            (modes.into_iter().map(S::Mode))
                .chain([S::NextMode])
                .collect(),
        ),
        ("Gig View", vec![S::GigView]),
        ("Looper", looper.into_iter().map(S::Looper).collect()),
        (
            "Page",
            [PageStep::Next, PageStep::Previous]
                .into_iter()
                .chain((0..pages).map(PageStep::Go))
                .map(S::CustomPage)
                .collect(),
        ),
    ];
    (groups.into_iter())
        .flat_map(|(group, actions)| {
            actions.into_iter().map(move |action| {
                let (name, detail) = mappings::system_text(action);
                let label = match action {
                    S::Tuner | S::GigView => name,
                    _ => detail,
                };
                json!({ "group": group, "label": label, "action": system_json(action) })
            })
        })
        .collect()
}
