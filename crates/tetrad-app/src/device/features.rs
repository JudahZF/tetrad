//! Device feature state and reducer helpers. C/I evidence: w3-research-dsp-features.

use std::collections::BTreeMap;
use std::time::Duration;

use tetrad_control::codecs::{
    ControlMessage, EnumValue, GlobalTempoMessage, GridMessage, MessageAction,
    MetronomeStatusUpdate, RawMessageBytes, TunerMessage,
};
use tetrad_core::performance::expression::{
    AutoEngage, BypassInfo, BypassSwitchType, ExpressionRange, Pedal as CorePedal,
    loaded_bypass_trim,
};
use tetrad_core::workflow::Millis;
use tetrad_dsp_protocol::CodecLimits;
use tetrad_dsp_protocol::controls::{ControlRequest, MetronomeSoundBundle};
use tetrad_dsp_protocol::responses::MetronomeStatus;
use tetrad_dsp_protocol::tuner_looper::{TunerEnableDestination, TunerLooperRequest};
use tetrad_engine::{Column, Pair, Row, Site, Slot};
use tetrad_hal::node::Pedal;
use tetrad_preset::{Model, Param, ParamValue, Preset, SceneValue};
use tetrad_ui::{Screen, SettingId, SettingValue};

use super::actions::{self, Action, ActionError};
use super::{DspCommand, Effect, State, Timer, preset};
use crate::tempo::{
    self, Echo, ParamRange, Source, TempoConfig, TempoController, TempoParam, TempoState,
};

pub const TUNER_POLL: Duration = Duration::from_millis(50);
pub const TUNER_IDLE: Duration = Duration::from_millis(500);
pub const METRONOME_FLUSH: Duration = Duration::from_millis(10);

/// `0x52098c`: `0x36` to destination 4, then 0. Off also clears the engine flag before the
/// lane I/O push; on sets it without a push. Boot adds [`tuner_path`] (`0x5b3924`). The
/// caller handles the meter poll ([`tuner_poll_changed`]).
pub fn tuner_engine(state: &mut State, enabled: bool) -> Vec<Effect> {
    state.tuner_engine = enabled;
    let mut effects = [TunerEnableDestination::Four, TunerEnableDestination::Zero]
        .map(|destination| {
            Effect::Dsp(DspCommand::TunerLooper(TunerLooperRequest::TunerEnable {
                destination,
                enabled,
            }))
        })
        .to_vec();
    if !enabled {
        effects.push(Effect::Dsp(DspCommand::LanePush));
    }
    effects
}

/// `0x87590c` then `0x519dfc`: the input port, then the lane mutes with [`State::tuner_muted`].
pub fn tuner_path(state: &State) -> [Effect; 2] {
    [
        Effect::Dsp(DspCommand::TunerLooper(
            TunerLooperRequest::GlobalTunerInputPath {
                port: state.settings.settings().tuner.input.to_stock() as u8,
            },
        )),
        Effect::Dsp(DspCommand::TunerMute),
    ]
}

/// The meter service polls `0x38` while it is on and has a consumer (`0x4e159c`,
/// `0x4e1274`). The Capture screen turns the service off with the engine (`0x3c9b94`).
/// Gig View is a consumer while it shows and LIVE TUNER is on (`liveTunerEnabledSlot`
/// `0x65debc`, hide `0x65c944`).
pub fn tuner_polls(state: &State) -> bool {
    state.tuner_engine && (tuner_view_shown(state) || state.tuner_meter_subscribed)
}

/// A view that draws the tuner reading is shown: the Tuner screen, or Gig View with LIVE
/// TUNER on.
pub fn tuner_view_shown(state: &State) -> bool {
    let gig = state.screen == Screen::Gig && state.settings.settings().tuner.live_tuner;
    state.screen == Screen::Tuner || gig
}

/// `0x4e159c` starts the 50 ms poll only when it is not running and stops only the poll;
/// the idle timer runs out on its own.
pub fn tuner_poll_changed(state: &State, polled: bool) -> Option<Effect> {
    match (polled, tuner_polls(state)) {
        (false, true) => Some(Effect::StartTimer {
            timer: Timer::TunerMeterPoll,
            after: TUNER_POLL,
        }),
        (true, false) => Some(Effect::StopTimer(Timer::TunerMeterPoll)),
        _ => None,
    }
}

/// `0x4e13d8`: a reading (≥ 0 Hz) equal to the last one is dropped; a new one restarts the
/// 500 ms idle timer. `None` is that timer running out (`0x4e12b0`): consumers get 0 Hz.
///
/// Only a shown tuner view repaints with it (the meter service feeds the views that consume
/// it, [`tuner_view_shown`]); a Cortex Control subscription alone costs no frame.
pub fn tuner_reading(state: &mut State, reading: Option<f32>) -> Vec<Effect> {
    let hz = match reading {
        Some(hz) if hz >= 0.0 && hz != state.tuner_hz => hz,
        Some(_) => return Vec::new(),
        None => 0.0,
    };
    state.tuner_hz = hz;
    let mut effects: Vec<Effect> = tuner_view_shown(state)
        .then_some(Effect::Present)
        .into_iter()
        .collect();
    if state.tuner_meter_subscribed {
        effects.push(Effect::ControlNotify(Box::new(ControlMessage::Tuner(
            TunerMessage {
                action: Some(EnumValue::Known(MessageAction::Update)),
                meter: Some(hz),
                ..Default::default()
            },
        ))));
    }
    if reading.is_some() {
        effects.push(Effect::StartTimer {
            timer: Timer::TunerMeterIdle,
            after: TUNER_IDLE,
        });
    }
    effects
}

/// The tuner screen opened (`0x3f0710` → `0x3ef3e8`) or closed (`0x3ecc48`). The metronome
/// level goes to 0 on open and back to `10^(slot 3 / 20)` on close (`0x557d00`); no flag
/// holds it at 0 in between, so later flushes send the real level. Closing keeps the mute
/// while the live tuner runs.
pub fn tuner_screen(state: &mut State, shown: bool) -> Vec<Effect> {
    let tuner = &state.settings.settings().tuner;
    state.tuner_muted = tuner.mutes_outputs && (shown || tuner.live_tuner);
    let level = |gain: f32| {
        Effect::Dsp(DspCommand::Control(ControlRequest::MetronomeParameter {
            destination: 1,
            parameter: 3,
            value_bits: gain.to_bits(),
        }))
    };
    if shown {
        let mut effects = vec![level(0.0), tuner_reference(state)];
        effects.extend(tuner_path(state));
        return effects;
    }
    let mut effects = Vec::new();
    if !state.settings.settings().tuner.live_tuner {
        effects.push(Effect::Dsp(DspCommand::TunerMute));
    }
    match state
        .metronome
        .as_ref()
        .and_then(|controller| controller.driver_value(TempoParam::LevelDb))
    {
        Some(db) => effects.push(level(10_f32.powf(db / 20.0))),
        None => effects.push(Effect::Notice(
            super::Subsystem::Dsp,
            "metronome level not restored after the tuner: slot 3 is unknown".into(),
        )),
    }
    effects
}

/// `0x3ef2a0` → `0x875e98`: `0x37` with 440 Hz + the stored offset.
fn tuner_reference(state: &State) -> Effect {
    Effect::Dsp(DspCommand::TunerLooper(
        TunerLooperRequest::TunerBaseFrequency {
            word: 0,
            reference_hz_bits: state.settings.settings().tuner.reference_hz().to_bits(),
        },
    ))
}

/// A stored tuner setting changed. Shown: the reference sends `0x37` (`0x3efc14`); input
/// and mute send [`tuner_path`] (`0x3ef6d4`, `0x3ef120`). Hidden: the live tuner toggle
/// sends the path (`0x6e0330`); input and mute do too while the live tuner runs (I);
/// otherwise the next open sends them.
pub fn tuner_setting_changed(state: &mut State, id: SettingId) -> Vec<Effect> {
    let tuner = &state.settings.settings().tuner;
    let (live, shown) = (tuner.live_tuner, state.screen == Screen::Tuner);
    state.tuner_muted = tuner.mutes_outputs && (shown || live);
    match id {
        SettingId::TunerReference if shown => vec![tuner_reference(state)],
        SettingId::TunerInput | SettingId::TunerMute if shown || live => tuner_path(state).to_vec(),
        SettingId::LiveTuner if !shown => tuner_path(state).to_vec(),
        _ => Vec::new(),
    }
}

/// P1 (`0x64c6e0`): index 1 and global scope read the settings. Preset scope reads the
/// loaded preset's 25000 block at the current scene in scene mode, else at its default scene.
pub fn scoped_value(state: &State, index: usize) -> Option<f32> {
    let settings = &state.settings.settings().tempo;
    if index == 1 || settings.params.get(&1).is_some_and(|v| v.round() != 0.0) {
        return settings.params.get(&(index as u16)).copied();
    }
    let current = state.current.as_ref()?;
    preset_tempo_value(&current.preset, current.deployment.scene().index(), index)
}

fn preset_tempo_value(preset: &Preset, scene: usize, index: usize) -> Option<f32> {
    let param = preset.tempo_program_data.first()?.params.get(index)?;
    let scene = if param.scene_mode.unwrap_or(false) {
        scene
    } else {
        preset.default_scene.unwrap_or(0).min(7) as usize
    };
    match param.value(scene)? {
        ParamValue::Float(v) => Some(*v),
        _ => None,
    }
}

/// Builds the controller from catalog ranges and the scoped stores; no invented ranges.
pub fn tempo_controller(state: &State) -> Result<TempoController, ActionError> {
    let mut ranges = [None; tempo::PARAM_COUNT];
    ranges[0] = Some(ParamRange::BPM);
    ranges[1] = Some(ParamRange::UNIT);
    if let Some(model) = state
        .specs
        .as_ref()
        .and_then(|specs| specs.models.get(&25_000))
    {
        for param in &model.parameters {
            if let Some(slot) = ranges.get_mut(param.index as usize)
                && let Some(range) = param.range
            {
                *slot = Some(ParamRange {
                    min: range.min,
                    max: range.max,
                    skew: param.skew.unwrap_or(1.0),
                });
            }
        }
    }
    let stored = &state.settings.settings().tempo.params;
    // An unset scope reads as preset scope (0x64c828 rounds 0).
    let global = std::array::from_fn(|index| {
        stored
            .get(&(index as u16))
            .copied()
            .or((index == 1).then_some(0.0))
    });
    let preset = state.current.as_ref().map(|current| &current.preset);
    let scene_mode = std::array::from_fn(|index| {
        preset
            .and_then(|preset| preset.tempo_program_data.first()?.params.get(index))
            .is_some_and(|param| param.scene_mode.unwrap_or(false))
    });
    let scenes = std::array::from_fn(|scene| {
        std::array::from_fn(|index| preset_tempo_value(preset?, scene, index))
    });
    let current_scene = state
        .current
        .as_ref()
        .map_or(0, |current| current.deployment.scene().index());
    let mut controller = TempoController::new(
        TempoConfig { ranges, scene_mode },
        TempoState {
            global,
            scenes,
            current_scene,
        },
    )?;
    if let Some(previous) = &state.metronome {
        let values: Option<Vec<_>> = (0..tempo::PARAM_COUNT)
            .map(|index| {
                previous.driver_value(TempoParam::from_index(index as u32).expect("tempo index"))
            })
            .collect();
        if let Some(values) = values {
            controller.initialize_driver(values.try_into().expect("tempo slots"))?;
        }
    }
    Ok(controller)
}

/// The scoped controller, built on first use.
fn metronome(state: &mut State) -> Result<&mut TempoController, ActionError> {
    if state.metronome.is_none() {
        state.metronome = Some(tempo_controller(state)?);
    }
    Ok(state.metronome.as_mut().expect("tempo controller"))
}

/// MIDI clock BPM (`0x556e3c` alone): the driver slot only, no store write and no echo. The
/// flush timer sends it in slot order (after an enable-off frame).
pub fn metronome_clock_tempo(state: &mut State, bpm: i32) -> Result<Vec<Effect>, ActionError> {
    let changed = metronome(state)?.set_driver(TempoParam::Bpm, bpm as f32)?;
    Ok(changed
        .then_some(Effect::StartTimer {
            timer: Timer::MetronomeFlush,
            after: METRONOME_FLUSH,
        })
        .into_iter()
        .collect())
}

pub fn set_tempo_parameter(
    state: &mut State,
    index: u32,
    value: f32,
    remote: bool,
) -> Result<Vec<Effect>, ActionError> {
    let controller = metronome(state)?;
    let old_signature = controller
        .driver_value(TempoParam::TimeSignature)
        .and_then(|real| tempo::TimeSignature::from_index(real as i32));
    let applied = controller.apply(
        index,
        value,
        if remote {
            Source::Remote
        } else {
            Source::Local
        },
    )?;
    if !applied.changed {
        return Ok(Vec::new());
    }
    let global_mode = controller.mode()? == tempo::TempoMode::Global;
    let to_global = global_mode && index == 1;
    let global = global_mode || index == 1;
    let echo = (applied.echo == Echo::GlobalTempo && !applied.guarded)
        .then(|| controller.global_tempo_update(applied.param))
        .flatten();
    let grid_echo = applied.echo == Echo::GridTempoProgramData && !applied.guarded;
    let armed = controller.flush_armed();
    let mut effects = Vec::new();
    if global {
        effects.extend(actions::update_settings(state, |settings| {
            settings.tempo.params.insert(index as u16, value);
        }));
    } else {
        let current = state
            .current
            .get_mut()
            .as_mut()
            .ok_or(ActionError::Unavailable("a loaded preset"))?;
        current
            .preset
            .tempo_program_data
            .first_mut()
            .ok_or(ActionError::Unavailable("the preset tempo block"))?
            .param_mut(index as usize)?
            .set_value(current.deployment.scene().index(), ParamValue::Float(value))?;
    }
    match applied.effect {
        Some(tempo::Effect::TempoChange { bpm }) => {
            effects.push(Effect::Dsp(DspCommand::Tempo(bpm)))
        }
        // `0x513410` → `0x512c74` on a scope change.
        Some(tempo::Effect::ResendAll) => effects.push(Effect::Action(Action::MetronomeResend)),
        _ => {}
    }
    let signature = match applied.effect {
        Some(tempo::Effect::TimeSignature(signature)) => signature,
        _ => None,
    };
    if armed {
        effects.push(Effect::StartTimer {
            timer: Timer::MetronomeFlush,
            after: METRONOME_FLUSH,
        });
    }
    if let Some(message) = echo {
        effects.push(Effect::ControlNotify(Box::new(
            ControlMessage::GlobalTempo(message),
        )));
    }
    if grid_echo {
        // `0x25f3f0`: one tempo model, one indexed parameter, one current value.
        let preset = Preset {
            tempo_program_data: vec![Model {
                params: vec![Param {
                    index: Some(index),
                    values: vec![SceneValue {
                        value: Some(ParamValue::Float(value)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        effects.push(Effect::ControlNotify(Box::new(ControlMessage::Grid(
            GridMessage {
                action: Some(EnumValue::Known(MessageAction::Update)),
                preset: Some(RawMessageBytes {
                    bytes: preset.to_bytes(),
                }),
                ..Default::default()
            },
        ))));
    }
    // `0x513410` index 6 → `0x494ab0(…, 1)` → `0x49469c`: the new row's default accents go
    // through this setter. Between two `x/4` rows only the added beats are written.
    if let Some(signature) = signature {
        let first = match old_signature {
            Some(old) if old.unit == 4 && signature.unit == 4 => usize::from(old.beats),
            _ => 0,
        };
        for (beat, level) in (0u32..).zip(signature.default_accents()).skip(first) {
            let index = 10 + beat;
            let value = tempo_range(state, index)?.normalize(f32::from(level));
            effects.extend(set_tempo_parameter(state, index, value, false)?);
        }
    }
    // `0x64c870`: a scope change to global while a MIDI clock runs copies its tempo
    // (`+0x104`, normalized over 40..240) into index 0.
    if to_global && state.midi_clock.running() {
        let bpm = ParamRange::BPM.normalize(state.midi_clock.tempo);
        effects.extend(set_tempo_parameter(state, 0, bpm, remote)?);
    }
    Ok(effects)
}

/// A DSP metronome status (`0x19`): stock drops an unchanged one (`0x4944bc`, `memcmp` of
/// the 7 words) and sends the rest as GlobalTempo UPDATE (`0x10ae84`). The status fields are
/// proto3 implicit, so a zero word is left out (stock 19–23 B per push in the S7 capture).
pub fn metronome_status(state: &mut State, status: MetronomeStatus) -> Vec<Effect> {
    if state.metronome_status == Some(status) {
        return Vec::new();
    }
    state.metronome_status = Some(status);
    let [enabled, preroll, beat, bar, tick, beats_left, bars_left] = status.words;
    let word = |value: u32| (value != 0).then_some(value);
    let signed = |value: u32| (value != 0).then_some(value as i32);
    // The beat dots are on the Tempo page only; other views would redraw an unchanged frame
    // at every beat.
    let present = (state.screen == Screen::Tempo).then_some(Effect::Present);
    present
        .into_iter()
        .chain([Effect::ControlNotify(Box::new(
            ControlMessage::GlobalTempo(GlobalTempoMessage {
                action: Some(EnumValue::Known(MessageAction::Update)),
                metronome_status: Some(MetronomeStatusUpdate {
                    is_enabled: word(enabled),
                    preroll_enabled: word(preroll),
                    current_beat: word(beat),
                    current_bar: word(bar),
                    current_tick: word(tick),
                    count_in_beats_remaining: signed(beats_left),
                    count_in_bars_remaining: signed(bars_left),
                }),
                ..Default::default()
            }),
        ))])
        .collect()
}

/// `0x512c74`: rebuild the scoped controller and plan the full resend. Every slot is
/// re-read from the store, so a pending MIDI clock BPM slot gives way to the stored BPM.
pub fn metronome_resend(state: &mut State) -> Result<tempo::ResendPlan, ActionError> {
    let mut controller = tempo_controller(state)?;
    let plan = controller.plan_resend()?;
    state.metronome = Some(controller);
    Ok(plan)
}

pub fn tempo_setting(
    state: &State,
    id: SettingId,
    value: &SettingValue,
) -> Result<Option<(u32, f32)>, ActionError> {
    let (index, real) = match (id, value) {
        (SettingId::TempoMode, SettingValue::Choice(v)) if *v <= 1 => (1, f32::from(1 - *v)),
        (SettingId::TempoLed, SettingValue::Bool(v)) => (2, f32::from(u8::from(*v))),
        (SettingId::Metronome, SettingValue::Bool(v)) => (4, f32::from(u8::from(*v))),
        (SettingId::TimeSignature, SettingValue::Choice(v)) => (6, f32::from(*v)),
        (SettingId::Subdivision, SettingValue::Choice(v)) => (7, f32::from(*v)),
        // Inferred: SOUND is slot 8 (sent raw, census catalog 25000 slot 8); the six choices
        // are the six kits of the bundle order.
        (SettingId::MetronomeSound, SettingValue::Choice(v)) => (8, f32::from(*v)),
        (SettingId::MetronomeRouting, SettingValue::Choice(v)) => (9, f32::from(*v)),
        (SettingId::MetronomeLevel, SettingValue::Number(v)) => (3, *v as f32 / 10.0),
        (SettingId::MetronomePan, SettingValue::Number(v)) => (5, *v as f32 / 100.0),
        (SettingId::BpmNote, _) => {
            return Err(ActionError::Unsupported(
                "BPM = (read-only: the time signature sets it)",
            ));
        }
        _ => return Ok(None),
    };
    Ok(Some((index, tempo_range(state, index)?.normalize(real))))
}

/// Time signatures before this index count quarters, the rest eighths (unit table
/// `0xa478f0`: 4 ×12, 8 ×9).
const EIGHTH_SIGNATURES: u16 = 12;

/// "BPM = QUARTERS / EIGHTHS" for the UI (tetrad-ui `SettingId::BpmNote` choice 0 / 1): the beat
/// unit of the scoped time signature (slot 6, `0x494ab0`). Read-only in stock.
pub fn bpm_note(state: &State) -> Option<u16> {
    let signature = tempo_range(state, 6)
        .ok()?
        .denormalize(scoped_value(state, 6)?);
    Some(u16::from(signature.round() as u16 >= EIGHTH_SIGNATURES))
}

/// Looper editor open (`0x614840` with 1) on a Looper X 27000: when parameter 20 (0..=3,
/// rounded) and parameter 18 (rounded) are not 0, stock asks once for the metronome status
/// (`0x875db0`: `0x45`, skipped under `/opt/neuraldsp/nopoll`).
pub fn looper_editor_opened(state: &State) -> Option<Effect> {
    let slot = looper_slot(state)?;
    let (specs, current) = (state.specs.as_ref()?, state.current.as_ref()?);
    let block = current.deployment.layout().instance(Site::Slot(slot))?;
    let scene = current.deployment.scene();
    let real = |index: u32| -> Option<f32> {
        let spec = specs.model(block.model.0)?.parameter(index)?;
        let range = spec.range?;
        let range = ParamRange {
            min: range.min,
            max: range.max,
            skew: spec.skew.unwrap_or(1.0),
        };
        Some(
            range
                .denormalize(block.params.get(index as usize)?.at(scene))
                .round(),
        )
    };
    let mode = (block.params.get(20)?.at(scene) * 3.0).round();
    (block.model.0 == 27000 && mode != 0.0 && real(18)? != 0.0).then_some(Effect::Dsp(
        DspCommand::Control(ControlRequest::MetronomeStatusRequest),
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum SoundUploadError {
    #[error(transparent)]
    Sound(#[from] tetrad_media::metronome::MetronomeSoundError),
    #[error(transparent)]
    Codec(#[from] tetrad_dsp_protocol::Error),
}

/// `MetronomeDriver::sendAllSounds` (`0x558554`, from the driver constructor `0x558ab8`): the
/// 18 stock click files under the factory root (`/opt/neuraldsp`) as one `0x47` bundle for
/// core 2. A file that cannot be read counts zero bytes, as stock; their names come back for
/// a notice.
pub fn metronome_sounds(
    factory: &std::path::Path,
) -> Result<(MetronomeSoundBundle, Vec<&'static str>), SoundUploadError> {
    let limits = CodecLimits::default();
    let sounds = tetrad_media::metronome::load_stock_sounds(
        |path| {
            let relative = path.strip_prefix("/opt/neuraldsp/")?;
            std::fs::read(factory.join(relative)).ok()
        },
        limits.max_message_bytes,
    )?;
    let missing = sounds
        .iter()
        .filter(|sound| sound.missing)
        .map(|sound| sound.file)
        .collect();
    let bundle = MetronomeSoundBundle::new(
        sounds.into_iter().map(|sound| sound.bytes).collect(),
        limits,
    )?;
    Ok((bundle, missing))
}

/// The catalog range of tempo slot `index` (model 25000); index 1 is the unit range.
fn tempo_range(state: &State, index: u32) -> Result<ParamRange, ActionError> {
    let spec = state
        .specs
        .as_ref()
        .and_then(|specs| specs.models.get(&25_000))
        .and_then(|model| model.parameters.get(index as usize));
    spec.and_then(|spec| spec.range)
        .map(|range| ParamRange {
            min: range.min,
            max: range.max,
            skew: spec.and_then(|spec| spec.skew).unwrap_or(1.0),
        })
        .or((index == 1).then_some(ParamRange::UNIT))
        .ok_or(ActionError::Unavailable("the tempo parameter range"))
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Expression {
    /// Continuous link values, and the 0 / 1 states of parameter switches.
    pub values: BTreeMap<(Site, u32), f32>,
    pub bypasses: BTreeMap<Site, bool>,
    assignments: BTreeMap<SwitchTarget, (usize, AutoEngage)>,
    deadlines: BTreeMap<SwitchTarget, Duration>,
}

/// What a pedal switch drives (`0x3242ec`): a block or mixer bypass (`bypass_expression`),
/// or a lane or input gate parameter that takes 1 (`true`) or 0 (`0x322988`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SwitchTarget {
    Bypass(Site),
    Param(Site, u32),
}

/// `0x3242ec`: a link on input gate parameter 1, lane MUTE (2) or lane SOLO (3) is a switch.
/// Returns its `expression_bypass_info` entry (gate `+0xbc`; lane `+0xc8`, `+0xd4`, load
/// order `0x680614`). Other parameter links are continuous.
pub fn switch_entry(site: Site, index: u32) -> Option<usize> {
    match (site, index) {
        (Site::InputControl(_), 1) | (Site::OutputControl(_), 2) => Some(0),
        (Site::OutputControl(_), 3) => Some(1),
        _ => None,
    }
}

pub fn pedal_index(pedal: Pedal) -> usize {
    match pedal {
        Pedal::One => 0,
        Pedal::Two => 1,
    }
}

fn assignment_index(raw: i32) -> Option<usize> {
    CorePedal::from_assignment(raw).map(|pedal| match pedal {
        CorePedal::Exp1 => 0,
        CorePedal::Exp2 => 1,
    })
}

/// A stored switch setting; none is the dialog default (Heel-Toe, 600 ms). Stock copies the
/// stored type raw (`0x323d9c` → `0x6df298`, inferred meaning); an unknown type here takes
/// the default with a notice, so the link still loads (review r11 N8).
fn stored_switch(
    info: Option<&tetrad_preset::ExpressionBypassInfo>,
    notices: &mut Vec<String>,
) -> BypassInfo {
    let Some(info) = info else {
        return BypassInfo::default();
    };
    let Some(kind) = BypassSwitchType::from_raw(info.kind) else {
        notices.push(format!(
            "expression switch type {} is unknown: the Heel-Toe default is used",
            info.kind
        ));
        return BypassInfo::default();
    };
    BypassInfo {
        kind,
        invert: info.invert,
        delay_ms: info.delay_ms,
        latch_emulation: info.latch_emulation,
    }
}

/// The parameters a pedal can drive: all but MUTE, the last stored one. `0x3242ec` skips
/// index count − 2 (`0x2ec444`) of its list, which also holds the bypass (review r11 N2).
fn linkable(model: &Model) -> impl Iterator<Item = (u32, &Param)> {
    let mute = model.params.len().saturating_sub(1);
    (0u32..).zip(&model.params[..mute])
}

/// A pedal link that cannot be sent: a notice, and the pass goes on (review r11 N2).
fn link_refused(error: &ActionError) -> Effect {
    Effect::Notice(
        error.subsystem(),
        format!("expression link not sent: {error}"),
    )
}

fn param_range(param: &Param) -> ExpressionRange {
    ExpressionRange {
        heel: param.expression_min.unwrap_or(0.0),
        toe: param.expression_max.unwrap_or(1.0),
    }
}

fn models(preset: &Preset) -> Vec<(Site, &Model)> {
    let mut models = Vec::new();
    for (row, chain) in preset.chains.iter().enumerate().take(4) {
        let row = Row::new(row as u8).expect("row");
        models.extend(
            chain
                .models
                .iter()
                .enumerate()
                .filter_map(|(column, model)| {
                    if model.is_empty() {
                        return None;
                    }
                    Some((
                        Site::Slot(Slot {
                            row,
                            column: Column::new(column as u8).ok()?,
                        }),
                        model,
                    ))
                }),
        );
        if let Some(model) = chain.input_control.first() {
            models.push((Site::InputControl(row), model));
        }
        if let Some(model) = chain.output_control.first() {
            models.push((Site::OutputControl(row), model));
        }
        if row.index().is_multiple_of(2) {
            let pair = if row.index() == 0 {
                Pair::Upper
            } else {
                Pair::Lower
            };
            if let Some(model) = chain
                .combined_splitter
                .first()
                .or_else(|| chain.splitter.first())
            {
                models.push((Site::Splitter(pair), model));
            }
            if let Some(model) = chain.mixer.first() {
                models.push((Site::Mixer(pair), model));
            }
        }
    }
    models
}

fn live_parameter(
    current: &super::Current,
    specs: &tetrad_catalog::Specs,
    site: Site,
    index: u32,
    value: f32,
) -> Result<Effect, ActionError> {
    Ok(match site {
        Site::InputControl(row) => Effect::DeployGate(preset::set_live_input_gate(
            current, specs, row, index, value,
        )?),
        site => Effect::Deploy(preset::set_live_parameter(
            current, specs, site, index, value,
        )?),
    })
}

/// The live state of a switch target: its override, else the deployed bypass or the stored
/// parameter at the current scene.
fn switch_state(
    values: &BTreeMap<(Site, u32), f32>,
    bypasses: &BTreeMap<Site, bool>,
    current: &super::Current,
    target: SwitchTarget,
) -> bool {
    let scene = current.deployment.scene();
    match target {
        SwitchTarget::Bypass(site) => bypasses.get(&site).copied().unwrap_or_else(|| {
            current
                .deployment
                .layout()
                .instance(site)
                .is_some_and(|block| block.bypass.at(scene))
        }),
        SwitchTarget::Param(site, index) => values
            .get(&(site, index))
            .copied()
            .or_else(|| preset::stored_parameter(&current.preset, site, index, scene.index()))
            .is_some_and(|value| value >= 0.5),
    }
}

/// Sends one switch output; only a good send becomes the target's override. A refusal is a
/// notice.
fn apply_switch(
    expression: &mut Expression,
    current: &super::Current,
    specs: &tetrad_catalog::Specs,
    target: SwitchTarget,
    on: bool,
) -> Effect {
    let sent = match target {
        SwitchTarget::Bypass(site) => live_bypass(current, specs, site, on).inspect(|_| {
            expression.bypasses.insert(site, on);
        }),
        SwitchTarget::Param(site, index) => {
            let value = f32::from(u8::from(on));
            live_parameter(current, specs, site, index, value).inspect(|_| {
                expression.values.insert((site, index), value);
            })
        }
    };
    sent.unwrap_or_else(|error| link_refused(&error))
}

/// The proposed preset's push overrides, before committing a deploy (`0x322108`), and a
/// notice detail per link that loads with a default.
pub fn expression_for(
    preset: &Preset,
    pedals: &[tetrad_core::performance::PedalState; 2],
    previous: &Expression,
) -> (Expression, Vec<String>) {
    let mut notices = Vec::new();
    let mut values = BTreeMap::new();
    let mut assignments = BTreeMap::new();
    let mut bypasses = BTreeMap::new();
    // `0x323d9c` with switch settings: the engage state survives an unchanged switch; the
    // first state is the static one (`0x6df7fc`).
    let mut engage = |target: SwitchTarget,
                      index: usize,
                      info: BypassInfo,
                      trim: ExpressionRange,
                      kept: Option<bool>| {
        let pedal = &pedals[index];
        let initial = pedal.position.unwrap_or(0.0);
        let engage = previous
            .assignments
            .get(&target)
            .filter(|(old, engage)| *old == index && engage.info == info && engage.trim == trim)
            .map_or_else(
                || AutoEngage::new(info, trim, initial),
                |(_, engage)| *engage,
            );
        assignments.insert(target, (index, engage));
        let position = if info.latch_emulation {
            pedal.emulated_switch()
        } else {
            initial
        };
        kept.unwrap_or_else(|| info.bypassed_at(position))
    };
    for (site, model) in models(preset) {
        for (index, param) in linkable(model) {
            let Some(pedal) = param
                .expression
                .and_then(assignment_index)
                .filter(|&pedal| pedals[pedal].is_active())
            else {
                continue;
            };
            match switch_entry(site, index) {
                Some(entry) => {
                    let target = SwitchTarget::Param(site, index);
                    let info = stored_switch(model.expression_bypass_info.get(entry), &mut notices);
                    let kept = previous
                        .assignments
                        .contains_key(&target)
                        .then(|| previous.values.get(&(site, index)).map(|v| *v >= 0.5))
                        .flatten();
                    let on = engage(target, pedal, info, param_range(param), kept);
                    values.insert((site, index), f32::from(u8::from(on)));
                }
                None => {
                    if let Some(position) = pedals[pedal].position {
                        values.insert((site, index), param_range(param).pushed_value(position));
                    }
                }
            }
        }
        // `0x680614` reads `bypass_expression` only outside the lane (23000) and the input
        // gate (28000) (helper read, review r11 N2).
        if !matches!(site, Site::InputControl(_) | Site::OutputControl(_))
            && let Some(assignment) = model.bypass_expression.first()
            && let Some(index) = assignment_index(assignment.expression)
            && pedals[index].is_active()
        {
            let info = stored_switch(model.expression_bypass_info.first(), &mut notices);
            let trim = loaded_bypass_trim(assignment.expression_min, assignment.expression_max);
            let kept = previous.bypasses.get(&site).copied();
            let bypassed = engage(SwitchTarget::Bypass(site), index, info, trim, kept);
            bypasses.insert(site, bypassed);
        }
    }
    let deadlines = previous
        .deadlines
        .iter()
        .filter(|(target, _)| assignments.contains_key(target))
        .map(|(&target, &deadline)| (target, deadline))
        .collect();
    let expression = Expression {
        values,
        assignments,
        bypasses,
        deadlines,
    };
    (expression, notices)
}

/// Rebuild after load/scene/unplug; live values stay separate from scene storage (`0x322108`).
/// Parameter switch states go out on a change like bypasses. A link that cannot be sent is
/// a notice; the other links still go out.
pub fn refresh_expression(state: &mut State, dispatch: bool) -> Vec<Effect> {
    let Some(current) = &*state.current else {
        return Vec::new();
    };
    let (mut next, notices) = expression_for(&current.preset, &state.pedals, &state.expression);
    let mut effects: Vec<Effect> = notices
        .into_iter()
        .map(|detail| Effect::Notice(super::Subsystem::Preset, detail))
        .collect();
    if let (Some(current), Some(specs)) = (state.current.as_ref(), state.specs.as_ref()) {
        let scene = current.deployment.scene();
        let mut sent = |effect: Result<Effect, ActionError>| match effect {
            Ok(effect) => {
                effects.push(effect);
                true
            }
            Err(error) => {
                effects.push(link_refused(&error));
                false
            }
        };
        for &(site, index) in state
            .expression
            .values
            .keys()
            .filter(|key| !next.values.contains_key(key))
        {
            if let Some(value) =
                preset::stored_parameter(&current.preset, site, index, scene.index())
            {
                sent(live_parameter(current, specs, site, index, value));
            }
        }
        for &site in state
            .expression
            .bypasses
            .keys()
            .filter(|site| !next.bypasses.contains_key(site))
        {
            if let Some(block) = current.deployment.layout().instance(site) {
                let bypass = block.bypass.at(scene);
                sent(live_bypass(current, specs, site, bypass));
            }
        }
        // Only a good send becomes an override; a refused one is tried again next time.
        next.values.retain(|&(site, index), &mut value| {
            let switch = next
                .assignments
                .contains_key(&SwitchTarget::Param(site, index));
            !((dispatch || switch) && state.expression.values.get(&(site, index)) != Some(&value))
                || sent(live_parameter(current, specs, site, index, value))
        });
        next.bypasses.retain(|&site, &mut value| {
            state.expression.bypasses.get(&site) == Some(&value)
                || sent(live_bypass(current, specs, site, value))
        });
    }
    state.expression = next;
    effects
}

fn live_bypass(
    current: &super::Current,
    specs: &tetrad_catalog::Specs,
    site: Site,
    bypassed: bool,
) -> Result<Effect, ActionError> {
    Ok(Effect::Deploy(preset::set_live_bypass(
        current, specs, site, bypassed,
    )?))
}

/// The next deadline timer of the Stop and Heel-Toe switches.
fn deadline_timer(expression: &Expression, now: Duration) -> Option<Effect> {
    let deadline = expression.deadlines.values().min()?;
    Some(Effect::StartTimer {
        timer: Timer::ExpressionBypass,
        after: deadline.saturating_sub(now),
    })
}

pub fn expression_event(
    state: &mut State,
    pedal: Pedal,
    position: f32,
    from_midi: bool,
    now: Duration,
) -> Vec<Effect> {
    if !position.is_finite() {
        return Vec::new();
    }
    let index = pedal_index(pedal);
    let Some(position) = state.pedals[index].update(position.clamp(0.0, 1.0), from_midi) else {
        return Vec::new();
    };
    let mut effects = refresh_expression(state, false);
    let edits: Vec<_> = state
        .current
        .as_ref()
        .map(|current| models(&current.preset))
        .unwrap_or_default()
        .into_iter()
        .flat_map(|(site, model)| {
            linkable(model).filter_map(move |(i, param)| {
                (param.expression.and_then(assignment_index) == Some(index)
                    && switch_entry(site, i).is_none())
                .then_some((site, i, param_range(param).live_value(position)))
            })
        })
        .collect();
    if let (Some(current), Some(specs)) = (state.current.as_ref(), state.specs.as_ref()) {
        for (site, parameter, value) in edits {
            match live_parameter(current, specs, site, parameter, value) {
                Ok(effect) => {
                    effects.push(effect);
                    // `0x520074` echoes each write to a connected Cortex Control at once.
                    if state.control_connected {
                        let echo = super::control::expression_echo(site, parameter, value);
                        effects.extend(echo.map(|echo| Effect::GridEcho(Box::new(echo))));
                    }
                    state.expression.values.insert((site, parameter), value);
                }
                Err(error) => effects.push(link_refused(&error)),
            }
        }
        let Expression {
            values,
            bypasses,
            assignments,
            deadlines,
        } = &mut state.expression;
        let mut switched = Vec::new();
        for (&target, (assigned, engage)) in assignments.iter_mut() {
            if *assigned != index {
                continue;
            }
            let on = switch_state(values, bypasses, current, target);
            if let Some(value) = engage.event(
                &state.pedals[index],
                position,
                from_midi,
                on,
                Millis(now.as_millis() as u64),
            ) {
                switched.push((target, value));
            }
            if engage.info.kind != BypassSwitchType::Switch {
                deadlines.insert(
                    target,
                    now + Duration::from_millis(u64::from(engage.info.delay_ms) + 1),
                );
            }
        }
        for (target, value) in switched {
            effects.push(apply_switch(
                &mut state.expression,
                current,
                specs,
                target,
                value,
            ));
        }
        if state.screen != Screen::Tempo
            && let Some(param) = current
                .preset
                .tempo_program_data
                .first()
                .and_then(|model| model.params.first())
                .filter(|param| param.expression.and_then(assignment_index) == Some(index))
        {
            let value = param_range(param).live_value(position);
            effects.push(Effect::Action(Action::SetTempo(
                (40.0 + value * 200.0).round(),
            )));
        }
    }
    effects.extend(deadline_timer(&state.expression, now));
    effects
}

pub fn expression_poll(state: &mut State, now: Duration) -> Vec<Effect> {
    let mut effects = Vec::new();
    if let (Some(current), Some(specs)) = (state.current.as_ref(), state.specs.as_ref()) {
        let expression = &state.expression;
        let due: Vec<_> = expression
            .assignments
            .iter()
            .filter(|(target, _)| {
                expression
                    .deadlines
                    .get(target)
                    .is_some_and(|deadline| *deadline <= now)
            })
            .map(|(&target, (index, engage))| {
                let on = switch_state(&expression.values, &expression.bypasses, current, target);
                let value = state.pedals[*index]
                    .position
                    .and_then(|p| engage.poll(p, on, Millis(now.as_millis() as u64)));
                (target, value)
            })
            .collect();
        for (target, value) in due {
            if let Some(value) = value {
                effects.push(apply_switch(
                    &mut state.expression,
                    current,
                    specs,
                    target,
                    value,
                ));
            }
            state.expression.deadlines.remove(&target);
        }
    }
    effects.extend(deadline_timer(&state.expression, now));
    effects
}

/// Caller gates of the Looper driver. Only buttons and the status reconcile read the clock,
/// and stock compares the metronome *driver* slot 4 there (`0x55743c`), not the store.
pub fn looper_context(state: &State) -> tetrad_dsp_host::setup::drivers::looper::LooperContext {
    use tetrad_dsp_host::setup::drivers::looper::{
        LooperClock, LooperContext, LooperMidiClockStart,
    };
    let midi = &state.settings.settings().midi;
    let slot_4 = state
        .metronome
        .as_ref()
        .and_then(|controller| controller.driver_value(TempoParam::Enabled));
    LooperContext {
        // `0x49a754`: only a full QC (platform 0 or 1) changes the P13–16 latches.
        mode_changes_allowed: Some(!state.platform.is_atma()),
        clock: slot_4.map(|metronome_slot_4| LooperClock {
            metronome_slot_4,
            midi_gate_f514: state.midi_clock.usb_running,
            midi_gate_fbb4: state.midi_clock.din_running,
        }),
        // `0x55e93c`: ClockInEn (`0x32dba4`) and ClockOutEn (`0x32db8c`).
        midi_clock_start: Some(LooperMidiClockStart {
            enabled: midi.clock_in,
            inhibited: midi.clock_out != tetrad_settings::midi::MidiClockOut::Off,
        }),
    }
}

/// The first deployed Looper X block (27000 or 27002).
pub fn looper_slot(state: &State) -> Option<Slot> {
    let current = state.current.as_ref()?;
    current
        .deployment
        .plan()
        .blocks
        .keys()
        .find(|&&slot| {
            current
                .deployment
                .layout()
                .instance(Site::Slot(slot))
                .is_some_and(|block| matches!(block.model.0, 27000 | 27002))
        })
        .copied()
}

/// A Looper tile edge from touch or a Looper-view footswitch (`0x6133c4`). The tile gates
/// tetrad-app can see, from the controller status copy `+0xb828`: only Record, Duplicate and
/// Punch In send a release (`0x612c60`); while an undo runs (`+0x185`) only Undo/Redo takes
/// a press; an empty loop (`+0x186`) drops Play/Stop and Reverse (`0x612a0c`), and Undo/Redo
/// when no redo exists (`0x6179ac`). In the idle state 1 an empty loop also marks Duplicate
/// and Punch In "Inactive" (`0x614e48`), and `0x6133c4` drops presses on Inactive tiles; the
/// tiles still draw white (shot 086). A passing edge is stored as P0–7 through the normal edit
/// path, which then runs the driver (`0x4b49cc` → `0x523644`). The rest of the per-state matrix
/// of `0x614e48` is not ported. MIDI CC#49..=56 skip the store ([`Action::Looper`]).
pub fn looper_tile(
    state: &State,
    button: tetrad_midi::LooperButton,
    pressed: bool,
) -> Result<Vec<Effect>, ActionError> {
    use tetrad_midi::LooperButton as B;
    let status = &state.looper_playback.incoming;
    let empty = status.loop_length == 0;
    let undoing = f32::from_bits(status.undo_progress_bits) > 0.0;
    let passes = if pressed {
        match button {
            B::Undo => undoing || !empty || status.redo_available != 0,
            _ if undoing => false,
            B::PlayStop | B::Reverse => !empty,
            B::Duplicate | B::PunchIn => !(empty && state.looper_playback.cached_state == 1),
            _ => true,
        }
    } else {
        matches!(button, B::RecordOverdub | B::Duplicate | B::PunchIn)
    };
    if !passes {
        return Ok(Vec::new());
    }
    let slot = looper_slot(state).ok_or(ActionError::Unavailable("a Looper X block"))?;
    Ok(vec![Effect::Action(Action::SetParameter {
        site: Site::Slot(slot),
        index: button as u32,
        value: f32::from(u8::from(pressed)),
    })])
}

/// Looper X swap dialog: footswitch `to` takes the action of footswitch `from` and the other
/// way round (`0x37ab08` → `stompAssignments`, saved by `0x651fa4`). Both are letters 0..8.
/// One letter press ends the assignment.
pub fn swap_looper_actions(state: &mut State, from: u8, to: u8) -> Vec<Effect> {
    state.looper_assign = None;
    let mut assignments = state.settings.settings().looper.stomp_assignments();
    assignments.swap(usize::from(from), usize::from(to));
    set_looper_actions(state, assignments)
}

/// Stores a permutation of the Looper X footswitch actions.
pub fn set_looper_actions(state: &mut State, assignments: [i32; 8]) -> Vec<Effect> {
    actions::update_settings(state, |settings| {
        let stored = settings.looper.set_stomp_assignments(assignments);
        debug_assert!(stored.is_ok(), "a swap or the factory set is a permutation");
    })
}
