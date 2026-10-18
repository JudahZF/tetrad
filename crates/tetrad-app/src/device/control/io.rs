//! Cortex Control IOSettings (type 3) over the live I/O values: UPDATE (`0x26acfc`, one change per
//! message), the one-port echo after every change (`0x26dafc`) and READ (`0x269ab4`; full dump
//! `0x17be68`). Values are the normalized `ConnectorsSettings` floats.

use tetrad_control::codecs::{
    ControlMessage, CortexMessageType, ExpPortSettings, HeadphonesFeedLevel, HeadphonesSettings,
    IOSettingsMessage, InputPortSettings, MIDIPortSettings, MessageAction, OutputPortSettings,
    PortSettings, USBPortSettings,
};
use tetrad_control::device::{DeviceError, DeviceResult, IoCommand, IoInputField, IoOutputField};
use tetrad_engine::SceneIndex;
use tetrad_hal::zenjack::Jack;
use tetrad_settings::io::ConnectorId;

use super::super::actions::Action;
use super::super::global_eq::IO_PRESET_MODEL;
use super::super::preset;
use super::super::ui::picker::preset_rows;
use super::StateModel;
use super::grid::SPECIAL_FACTORY_PRESET;

const IO: CortexMessageType = CortexMessageType::IOSettings;
use super::super::jacks::OutputPair;
use super::super::{Effect, State};

/// One input port (`GainCalInputPortId`) and its ids: level, impedance, TYPE + PHANTOM,
/// ground lift (`0x17b5d8`), and its jack detect (`0x67ab94`, map `_INIT_552`).
struct InputPort {
    port: u32,
    jack: Jack,
    level: ConnectorId,
    zmode: Option<ConnectorId>,
    kind: Option<(ConnectorId, ConnectorId)>,
    lift: ConnectorId,
}

/// One output port (`GainCalOutputPortId`) and its ids (`0x17b870`). `plugged` is set when
/// every jack of `jacks` is plugged (`0x67aef0`, map `_INIT_552`); an empty list writes no flag.
struct OutputPort {
    port: u32,
    jacks: &'static [Jack],
    level: ConnectorId,
    lift: Option<ConnectorId>,
    mute: Option<ConnectorId>,
}

const INPUTS: [InputPort; 4] = {
    use ConnectorId::*;
    [
        InputPort {
            port: 1,
            jack: Jack::Input1,
            level: In1Level,
            zmode: Some(In1Impedance),
            kind: Some((In1Type, In1Phantom)),
            lift: In1GroundLift,
        },
        InputPort {
            port: 2,
            jack: Jack::Input2,
            level: In2Level,
            zmode: Some(In2Impedance),
            kind: Some((In2Type, In2Phantom)),
            lift: In2GroundLift,
        },
        InputPort {
            port: 4,
            jack: Jack::Return1,
            level: Ret1Level,
            zmode: None,
            kind: None,
            lift: Ret1GroundLift,
        },
        InputPort {
            port: 5,
            jack: Jack::Return2,
            level: Ret2Level,
            zmode: None,
            kind: None,
            lift: Ret2GroundLift,
        },
    ]
};

/// In the full-dump order of `0x17be68`.
const OUTPUTS: [OutputPort; 8] = {
    use ConnectorId::*;
    [
        OutputPort {
            port: 4,
            jacks: &[],
            level: Out1Level,
            lift: Some(Out1GroundLift),
            mute: Some(Out1Mute),
        },
        OutputPort {
            port: 5,
            jacks: &[],
            level: Out2Level,
            lift: Some(Out2GroundLift),
            mute: Some(Out2Mute),
        },
        OutputPort {
            port: 1,
            jacks: &[],
            level: Out12Level,
            lift: Some(Out12GroundLift),
            mute: Some(Out12Mute),
        },
        OutputPort {
            port: 6,
            jacks: &[Jack::Output6],
            level: Out3Level,
            lift: None,
            mute: Some(Out3Mute),
        },
        OutputPort {
            port: 7,
            jacks: &[Jack::Output7],
            level: Out4Level,
            lift: None,
            mute: Some(Out4Mute),
        },
        OutputPort {
            port: 2,
            jacks: &[Jack::Output6, Jack::Output7],
            level: Out34Level,
            lift: None,
            mute: Some(Out34Mute),
        },
        OutputPort {
            port: 8,
            jacks: &[Jack::Output8],
            level: Send1Level,
            lift: None,
            mute: None,
        },
        OutputPort {
            port: 9,
            jacks: &[Jack::Output9],
            level: Send2Level,
            lift: None,
            mute: None,
        },
    ]
};

fn feed(port: u32) -> Option<ConnectorId> {
    ConnectorId::QC_TABLE
        .into_iter()
        .find(|id| id.hp_feed_port().map(u32::from) == Some(port))
}

/// `0x355b3c`: a non-ESS QC input on TS (TYPE < 0.5) has no 48 V and takes the impedance.
fn instrument(state: &State, type_id: ConnectorId) -> bool {
    !state.platform.has_ess_codec() && state.io.value(type_id) < 0.5
}

/// The actions of one UPDATE (`0x26acfc`). `0x35f75c` refuses impedance on Mic and phantom on
/// TS, on non-ESS boards only; a refused value is skipped as stock does.
pub(super) fn update(state: &State, command: IoCommand) -> Result<Vec<Action>, String> {
    let set = |id, value| Action::SetConnector { id, value };
    Ok(match command {
        IoCommand::Input { port, field } => {
            let input = INPUTS
                .iter()
                .find(|input| input.port == port)
                .ok_or_else(|| format!("input port {port}"))?;
            match field {
                IoInputField::GroundLift(v) => vec![set(input.lift, v)],
                IoInputField::Level(v) => vec![set(input.level, v)],
                IoInputField::Zmode(v) => match (input.zmode, input.kind) {
                    (Some(zmode), Some((type_id, _)))
                        if state.platform.has_ess_codec() || instrument(state, type_id) =>
                    {
                        vec![set(zmode, v)]
                    }
                    _ => Vec::new(),
                },
                // `round(2v)`: 0 TS, 1 XLR, 2 XLR+48V; TYPE first, then PHANTOM.
                IoInputField::InputType(v) => {
                    let Some((type_id, phantom)) = input.kind else {
                        return Ok(Vec::new());
                    };
                    let n = (v.clamp(0.0, 1.0) * 2.0).round();
                    let mic = f32::from(u8::from(n >= 1.0));
                    let mut actions = vec![set(type_id, mic)];
                    if state.platform.has_ess_codec() || mic >= 0.5 {
                        actions.push(set(phantom, f32::from(u8::from(n >= 2.0))));
                    }
                    actions
                }
            }
        }
        IoCommand::Output { port, field } => {
            let output = OUTPUTS
                .iter()
                .find(|output| output.port == port)
                .ok_or_else(|| format!("output port {port}"))?;
            let id = match field {
                IoOutputField::GroundLift(v) => output.lift.map(|id| (id, v)),
                IoOutputField::Level(v) => Some((output.level, v)),
                IoOutputField::Mute(mute) => output.mute.map(|id| (id, f32::from(u8::from(mute)))),
            };
            id.map(|(id, v)| set(id, v)).into_iter().collect()
        }
        IoCommand::HeadphoneLevel(v) => vec![set(ConnectorId::HpLevel, v)],
        IoCommand::HeadphoneFeed { port, level } => {
            vec![set(
                feed(port).ok_or_else(|| format!("headphone feed port {port}"))?,
                level,
            )]
        }
        IoCommand::UsbHeadphoneSelect(v) => vec![set(ConnectorId::HpSource, v)],
        IoCommand::UsbLevel(v) => vec![set(ConnectorId::UsbLevel, v)],
        IoCommand::UsbDryWet(v) => vec![set(ConnectorId::UsbDryWet, v)],
        IoCommand::MidiThru(v) => vec![set(ConnectorId::MidiThru, v)],
        IoCommand::Xlr12Linked(linked) => vec![Action::PairOutputs {
            pair: OutputPair::Out12,
            linked,
        }],
        IoCommand::Out34Linked(linked) => vec![Action::PairOutputs {
            pair: OutputPair::Out34,
            linked,
        }],
        IoCommand::ExpCalibrating { .. } => {
            return Err("an EXP calibration goes to the input bridge".into());
        }
        IoCommand::LoadPreset(_) => return Err("an I/O preset goes through `load_preset`".into()),
    })
}

/// The EXP calibration rows of a 31000 preset (EXP 1 MIN and MAX, then EXP 2; map
/// `0x1548748`). They hold raw ADC counts (catalog `int`, 0..255; `0x361c3c`).
const CALIBRATION_ROWS: std::ops::RangeInclusive<i64> = 48..=51;

/// `preset_to_load` `0x3634e8`: a stored preset becomes a fresh 31000 block with its rows
/// (`0x1f7e74`; zenui.db, or factory.db when `is_factory`), loaded whole (`0x362d18`, then the
/// reload `0x360da8` and the EXP calibration `0x320fdc`): [`Action::LoadSettingsPreset`] with
/// every block row and the stored calibration rows unchanged (the block keeps only values in
/// 0..1). The special id is the factory default (`0x3615a0`): [`Action::LoadFactoryIoPreset`].
/// The `0x511904` wrapper is in `Action::LoadSettingsPreset` (r19 F2). Not built: the
/// `0x358698` refresh.
pub(super) fn load_preset(
    model: &StateModel<'_>,
    preset: &tetrad_control::codecs::ModelPresetID,
) -> DeviceResult<Action> {
    let refused = |what: &str| DeviceError::Refused(IO, what.to_owned());
    if preset.value.as_deref() == Some(SPECIAL_FACTORY_PRESET) {
        return Ok(Action::LoadFactoryIoPreset);
    }
    let stored = super::settings::stored_preset_rows(model, preset, IO)?;
    let specs = (model.state.specs.as_ref()).ok_or_else(|| refused("no catalog"))?;
    let block = preset::model_preset_block(specs, IO_PRESET_MODEL, None, &stored)
        .ok_or_else(|| refused("no I/O Settings model (31000)"))?;
    let scene = SceneIndex::new(0).expect("scene 0");
    let (_, mut rows) = preset_rows(&block, None, scene);
    rows.retain(|row| !CALIBRATION_ROWS.contains(&row.index));
    rows.extend((stored.into_iter()).filter(|row| CALIBRATION_ROWS.contains(&row.index)));
    Ok(Action::LoadSettingsPreset {
        model: IO_PRESET_MODEL,
        rows,
    })
}

/// The full IOSettings push `0x26ebc0` → `0x17be68`: the `read` dump as UPDATE. Stock sends
/// it after an I/O reload or reset (`0x360da8`, `0x3615a0`) instead of the per-port echoes;
/// the link sends it only while connected (`0x6e0444`).
pub fn push(state: &State) -> Effect {
    Effect::ControlNotify(Box::new(ControlMessage::IOSettings(IOSettingsMessage {
        action: Some(MessageAction::Update.into()),
        ..read(state, &IOSettingsMessage::default())
    })))
}

/// The one-port UPDATE after a change (`0x26dafc`; the link gates it on the connected
/// session, `0x6e0444`). Impedance echoes `index/8` (`0x26e80c`), TYPE `index/2`, PHANTOM 1.0
/// or 0.5 (`0x26cf2c`), a mute
/// `v ≥ 0.5` (`0x26d0a0`). The stock 100 ms delay for remote level and impedance edits
/// (`0x26d864`) is not modelled.
pub fn echo(state: &State, id: ConnectorId) -> Option<Effect> {
    let io = &state.io;
    let v = io.value(id);
    let command = if let Some(input) = INPUTS.iter().find(|input| {
        [Some(input.level), input.zmode, Some(input.lift)].contains(&Some(id))
            || input
                .kind
                .is_some_and(|(type_id, phantom)| id == type_id || id == phantom)
    }) {
        let index = io.index(id).unwrap_or(0) as f32;
        let field = if id == input.level {
            IoInputField::Level(v)
        } else if id == input.lift {
            IoInputField::GroundLift(v)
        } else if input.zmode == Some(id) {
            IoInputField::Zmode(index / 8.0)
        } else if input.kind.is_some_and(|(type_id, _)| id == type_id) {
            IoInputField::InputType(index / 2.0)
        } else {
            IoInputField::InputType(if index >= 1.0 { 1.0 } else { 0.5 })
        };
        IoCommand::Input {
            port: input.port,
            field,
        }
    } else if let Some(output) = OUTPUTS
        .iter()
        .find(|output| [Some(output.level), output.lift, output.mute].contains(&Some(id)))
    {
        let field = if id == output.level {
            IoOutputField::Level(v)
        } else if output.lift == Some(id) {
            IoOutputField::GroundLift(v)
        } else {
            IoOutputField::Mute(v >= 0.5)
        };
        IoCommand::Output {
            port: output.port,
            field,
        }
    } else if let Some(port) = id.hp_feed_port() {
        IoCommand::HeadphoneFeed {
            port: u32::from(port),
            level: v,
        }
    } else {
        match id {
            ConnectorId::HpLevel => IoCommand::HeadphoneLevel(v),
            ConnectorId::UsbLevel => IoCommand::UsbLevel(v),
            ConnectorId::HpSource => IoCommand::UsbHeadphoneSelect(v),
            ConnectorId::UsbDryWet => IoCommand::UsbDryWet(v),
            ConnectorId::MidiThru => IoCommand::MidiThru(v),
            _ => return None,
        }
    };
    Some(notify(command))
}

/// The link flag echo after a pair change.
pub fn pair_echo(pair: OutputPair, linked: bool) -> Effect {
    notify(match pair {
        OutputPair::Out12 => IoCommand::Xlr12Linked(linked),
        OutputPair::Out34 => IoCommand::Out34Linked(linked),
    })
}

fn notify(command: IoCommand) -> Effect {
    Effect::ControlNotify(Box::new(ControlMessage::IOSettings(command.to_update())))
}

/// READ (`0x269ab4`). An empty request gets the full dump of `0x17be68`: inputs 1, 2, 4, 5,
/// outputs 4, 5, 1, 6, 7, 2, 8, 9, headphones with one feed per preset row output
/// (`0x3597ac`), MIDI, USB, both EXP ports and both link flags. A request that lists ports
/// keeps only those ports and the headphones only when asked (I: the mirror rule is read
/// from the summary, not the body). `usb_port.plugged` is the USB detect (`0x67aa24`): set,
/// as Cortex Control reaches the device over USB (S7 capture: 1, C). EXP `calibrating`
/// (`0x358260(.., n)`) is written for both ports (presence field); the calibration runs
/// outside device state, so it is false (I). The jack `plugged` flags read the HAL jack
/// word (`0x67b3b8`, [`State::jacks`]): inputs 1, 2, 4, 5 (`0x67ab94`), outputs 6, 7, 8, 9
/// (`0x67aef0`), output 2 only with both 6 and 7, headphones bit 12 (`0x67aa08`). Outputs 4,
/// 5 and 1 have no flag. A request that lists output 2 gets `plugged` false: its builder
/// `0x2683cc` looks port 2 up in the 6..9 map (`0x67aef0(.., 2)`), which has no entry.
pub(super) fn read(state: &State, request: &IOSettingsMessage) -> IOSettingsMessage {
    let io = &state.io;
    let hardware = io.hardware();
    let present = |id: ConnectorId| id.spec(hardware).is_some().then(|| io.value(id));
    let in_port = INPUTS
        .iter()
        .map(|input| InputPortSettings {
            input_port_id: Some(input.port),
            level: present(input.level),
            input_zmode: input.zmode.and_then(present),
            input_type: input.kind.map(|(type_id, phantom)| {
                if instrument(state, type_id) {
                    0.0
                } else if io.value(phantom) >= 0.5 {
                    1.0
                } else {
                    0.5
                }
            }),
            ground_lift: present(input.lift),
            plugged: Some(state.jacks.plugged(input.jack)),
        })
        .collect();
    let out_port = OUTPUTS
        .iter()
        .map(|output| OutputPortSettings {
            output_port_id: Some(output.port),
            level: Some(io.value(output.level)),
            ground_lift: output.lift.map(|id| io.value(id)),
            mute: output.mute.map(|id| io.value(id) > 0.0),
            plugged: (!output.jacks.is_empty())
                .then(|| output.jacks.iter().all(|&jack| state.jacks.plugged(jack))),
        })
        .collect();
    let hp_feed = state
        .current
        .iter()
        .flat_map(|current| current.deployment.layout().lanes.iter())
        .filter_map(|lane| feed(lane.output as u32))
        .map(|id| HeadphonesFeedLevel {
            level: Some(io.value(id)),
            output_port_id: id.hp_feed_port().map(u32::from),
        })
        .collect();
    let exp_port = (1..=2)
        .zip(&state.pedals)
        .map(|(id, pedal)| ExpPortSettings {
            exp_port_id: Some(id),
            plugged: Some(pedal.plugged),
            level: pedal.position,
            calibrating: Some(false),
        })
        .collect();
    let settings = state.settings.settings();
    let mut reply = IOSettingsMessage {
        settings: Some(PortSettings {
            in_port,
            out_port,
            hp_port: Some(HeadphonesSettings {
                level: Some(io.value(ConnectorId::HpLevel)),
                hp_feed,
                plugged: Some(state.jacks.plugged(Jack::Headphones)),
            }),
            usb_port: Some(USBPortSettings {
                level: Some(io.value(ConnectorId::UsbLevel)),
                hp_select: Some(io.value(ConnectorId::HpSource)),
                plugged: Some(true),
                dry_wet: Some(io.value(ConnectorId::UsbDryWet)),
            }),
            midi_port: Some(MIDIPortSettings {
                midi_thru: Some(io.value(ConnectorId::MidiThru)),
            }),
            exp_port,
        }),
        xlr1_2_linked: Some(settings.io.out12_paired),
        out3_4_linked: Some(settings.io.out34_paired),
        ..Default::default()
    };
    if let (Some(asked), Some(full)) = (&request.settings, reply.settings.as_mut())
        && (!asked.in_port.is_empty() || !asked.out_port.is_empty() || asked.hp_port.is_some())
    {
        let inputs: Vec<_> = asked.in_port.iter().map(|p| p.input_port_id).collect();
        let outputs: Vec<_> = asked.out_port.iter().map(|p| p.output_port_id).collect();
        full.in_port.retain(|p| inputs.contains(&p.input_port_id));
        full.out_port
            .retain(|p| outputs.contains(&p.output_port_id));
        for port in &mut full.out_port {
            if port.output_port_id == Some(2) {
                port.plugged = Some(false);
            }
        }
        if asked.hp_port.is_none() {
            full.hp_port = None;
        }
    }
    reply
}
