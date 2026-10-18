//! The I/O SETTINGS page behind the UI and Cortex Control: live [`Connectors`] values, their apply
//! paths ([`set`], `0x35476c` levels, `0x35b494` choices), the boot restore ([`restore`],
//! `0x35c4ac`), the reset ([`reset`], `0x3615a0`) and the 250 ms save ([`save`]). Gain, impedance
//! and ESS type changes go through one muted input scheduler and timer (`0x7d65c4` start,
//! `0x7d7450` timer slot). States follow the stock `+0x8` numbers; delays are the `QTimer::start`
//! arguments in ARM.
//!
//! Each HAL write carries the stock `readback, commgap` flags of its caller
//! (`setParameterValue(int,float,bool,bool)` `0x35476c`, `setStepIndex(int,int,bool,bool)`
//! `0x35ac14`): only the boot restore verifies ([`ZENUI_VERIFY`](tetrad_hal::zenjack::ZENUI_VERIFY): `0x35e6d4`, `0x35b068`). An
//! edit (`0x354c74`, `0x35b8e8`), the reload (`0x360da8`), the reset (`0x3615a0`), the pairs
//! (`0x3608e8`) and the Neural Capture I/O (`0x1ff730`, `0x1ff190`) write unverified, so they
//! do not hold a pass for the 25 ms read-back pauses.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use tetrad_dsp_protocol::requests::Request;
use tetrad_dsp_protocol::{CoreId, DspCore};
use tetrad_hal::analog::{GainEndpoint, Verification};
use tetrad_hal::codec::Impedance;
use tetrad_hal::hal::{Command, Hal, HalError, Query, Reading};
use tetrad_hal::platform::Platform;
use tetrad_hal::zenjack::{
    Boost, Control, GainPlan, GroundLift, HeadphoneVolume, Input, InputType, Return, Setting,
    Value, XlrOut, ZenjackError,
};
use tetrad_settings::device::{Hardware, Settings};
use tetrad_settings::io::{ComboJack, ConnectorId, ExpressionRange, output_gain, usb_gain};
use tetrad_storage::stock::ParamEntry;
use tetrad_ui::{SettingId, SettingValue};

use super::{Action, DspCommand, Effect, State, Subsystem, Timer};

/// `+0xd = 1`: zeros to all four trims.
const MUTE: Effect = Effect::Dsp(DspCommand::InputTrims(None));

/// A scheduler request. Each queued request keeps the `readback, commgap` flags of its caller
/// (bytes `+8`, `+9` of the stock queue entries, `0x7d7ce8`, `0x7d81e0`, `0x7d8878`); its HAL
/// write uses them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputChange {
    /// The external mute `+0xc` (`0x7d6440`): while on, every trim write is zero.
    ExternalMute(bool),
    Type {
        input: Input,
        kind: InputType,
        verification: Verification,
    },
    Impedance {
        input: Input,
        impedance: Impedance,
        verification: Verification,
    },
    Gain {
        endpoint: GainEndpoint,
        plan: GainPlan,
        verification: Verification,
    },
}

#[derive(Debug, Default)]
enum Step {
    #[default]
    Idle,
    /// 1: muted; the PGA write waits for the timer.
    Gain(GainEndpoint, GainPlan, Verification),
    /// 2: gains written (or unchanged); next the type queue.
    TypesReady,
    /// 3: muted; the impedance write waits for the timer.
    Impedance(Input, Impedance, Verification),
    /// 4: impedance settled; unmute.
    ImpedanceSettled,
    /// 5: muted; zero impedances and write every queued type.
    TypeSwitch,
    /// 6: restore the impedances.
    TypeRestore,
    /// 7: unmute.
    Unmute,
}

#[derive(Debug)]
pub struct InputScheduler {
    platform: Platform,
    step: Step,
    gains: VecDeque<(GainEndpoint, GainPlan, Verification)>,
    impedances: VecDeque<(Input, Impedance, Verification)>,
    types: VecDeque<(Input, InputType, Verification)>,
    /// `+0x9c`, `+0x9d`: the flags of the last impedance request taken from the queue. The
    /// type switch writes its zero and restored impedances with them (`0x7d7450` cases 5, 6).
    /// I: stock does not set them in the constructor; Tetrad starts unverified.
    impedance_verification: Verification,
    /// `+0xdc`: last written PGA step per trim slot; stock starts at −1 (`0x7d8dc0`).
    cached_step: [Option<i32>; 4],
    cached_type: [Option<InputType>; 2],
    cached_z: [Option<Impedance>; 2],
    /// `+0xec..+0xf8`, inputs 1, 2 and returns 1, 2.
    trims: [f32; 4],
    /// `+0xd`: the scheduler's own mute.
    muted: bool,
    /// `+0xc`: the external mute.
    external_mute: bool,
}

impl InputScheduler {
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            step: Step::Idle,
            gains: VecDeque::new(),
            impedances: VecDeque::new(),
            types: VecDeque::new(),
            impedance_verification: Verification::Unverified,
            cached_step: [None; 4],
            cached_type: [None; 2],
            cached_z: [None; 2],
            // Stock starts at 0; the boot restore queues every gain and fills them.
            trims: [0.0; 4],
            muted: false,
            external_mute: false,
        }
    }

    /// Queues one change and starts the scheduler when idle. The first type change observes
    /// the cached type and, on QC, both impedances; a failed read refuses it before muting.
    pub fn enqueue(
        &mut self,
        change: InputChange,
        hal: &mut impl Hal,
    ) -> Result<Vec<Effect>, HalError> {
        match change {
            InputChange::ExternalMute(on) => {
                self.external_mute = on;
                return Ok(vec![self.trims()]);
            }
            InputChange::Type {
                input,
                kind,
                verification,
            } => {
                if self.cached_type[input as usize].is_none() {
                    let Value::InputType(value) = observe(hal, Control::InputType(input))? else {
                        return Err(HalError::NoReading(Query::Setting(Control::InputType(
                            input,
                        ))));
                    };
                    self.cached_type[input as usize] = Some(value);
                }
                if self.platform.is_qc() {
                    for input in [Input::One, Input::Two] {
                        self.observe_impedance(hal, input)?;
                    }
                }
                self.types.push_back((input, kind, verification));
            }
            // Stock drops impedance work off QC (`0x49a754`), so mini never queues it.
            InputChange::Impedance {
                input,
                impedance,
                verification,
            } if self.platform.is_qc() => {
                self.observe_impedance(hal, input)?;
                self.impedances.push_back((input, impedance, verification));
            }
            InputChange::Impedance { .. } => {}
            InputChange::Gain {
                endpoint,
                plan,
                verification,
            } => self.gains.push_back((endpoint, plan, verification)),
        }
        Ok(self.start())
    }

    pub fn on_timer(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        match std::mem::take(&mut self.step) {
            Step::Idle => {}
            // State 1: write this gain and every queued gain, then every queued impedance
            // once, then wait 50 ms (QC) or 1 ms (mini) for the types (`0x7d7738`).
            Step::Gain(endpoint, plan, verification) => {
                self.write_gain(endpoint, plan, verification, &mut effects);
                while let Some((endpoint, plan, verification)) = self.gains.pop_front() {
                    self.write_gain(endpoint, plan, verification, &mut effects);
                }
                while let Some((input, impedance, verification)) = self.impedances.pop_front() {
                    self.impedance_verification = verification;
                    if self.cached_z[input as usize] != Some(impedance) {
                        effects.push(write(
                            Control::InputImpedance(input),
                            Value::Impedance(impedance),
                            verification,
                        ));
                        self.cached_z[input as usize] = Some(impedance);
                    }
                }
                self.step = Step::TypesReady;
                effects.push(timer(if self.platform.is_qc() { 50 } else { 1 }));
            }
            Step::TypesReady => {
                if !self.start_types(&mut effects) {
                    effects.push(self.unmute());
                    effects.extend(self.start());
                }
            }
            Step::Impedance(input, impedance, verification) => {
                effects.push(write(
                    Control::InputImpedance(input),
                    Value::Impedance(impedance),
                    verification,
                ));
                self.cached_z[input as usize] = Some(impedance);
                self.step = Step::ImpedanceSettled;
                effects.push(timer(25));
            }
            Step::TypeSwitch => {
                if self.platform.is_qc() {
                    let zero = Impedance::try_from(0).expect("zero is an impedance code");
                    for input in [Input::One, Input::Two] {
                        effects.push(write(
                            Control::InputImpedance(input),
                            Value::Impedance(zero),
                            self.impedance_verification,
                        ));
                    }
                }
                while let Some((input, kind, verification)) = self.types.pop_front() {
                    if self.cached_type[input as usize] != Some(kind) {
                        effects.push(write(
                            Control::InputType(input),
                            Value::InputType(kind),
                            verification,
                        ));
                        self.cached_type[input as usize] = Some(kind);
                    }
                }
                self.step = Step::TypeRestore;
                effects.push(timer(500));
            }
            Step::TypeRestore => {
                if self.platform.is_qc() {
                    for input in [Input::One, Input::Two] {
                        let value = self.cached_z[input as usize].expect("observed at enqueue");
                        effects.push(write(
                            Control::InputImpedance(input),
                            Value::Impedance(value),
                            self.impedance_verification,
                        ));
                    }
                }
                self.step = Step::Unmute;
                effects.push(timer(25));
            }
            Step::ImpedanceSettled | Step::Unmute => {
                effects.push(self.unmute());
                effects.extend(self.start());
            }
        }
        effects
    }

    /// `0x7d65c4`: runs only when idle. Gains first, then impedances, then types.
    fn start(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        if !matches!(self.step, Step::Idle) {
            return effects;
        }
        if let Some((endpoint, plan, verification)) = self.gains.pop_front() {
            let slot = slot(endpoint);
            self.trims[slot] = plan.trim;
            // An unchanged PGA step skips the mute and the write; the unmute applies the trim.
            self.step = if self.cached_step[slot] == Some(plan.step) {
                Step::TypesReady
            } else {
                self.muted = true;
                effects.push(MUTE);
                Step::Gain(endpoint, plan, verification)
            };
            effects.push(timer(if self.platform.is_qc() { 10 } else { 1 }));
            return effects;
        }
        while let Some((input, impedance, verification)) = self.impedances.pop_front() {
            self.impedance_verification = verification;
            if self.cached_z[input as usize] != Some(impedance) {
                self.step = Step::Impedance(input, impedance, verification);
                self.muted = true;
                return vec![MUTE, timer(1)];
            }
        }
        self.start_types(&mut effects);
        effects
    }

    /// State 0 or 2: skip types equal to the cache; mute and wait 20 ms for the switch.
    /// Returns false when no type is left (state 2 then unmutes, `0x7d7378`).
    fn start_types(&mut self, effects: &mut Vec<Effect>) -> bool {
        while self
            .types
            .front()
            .is_some_and(|&(input, kind, _)| self.cached_type[input as usize] == Some(kind))
        {
            self.types.pop_front();
        }
        if self.types.is_empty() {
            return false;
        }
        self.step = Step::TypeSwitch;
        self.muted = true;
        effects.extend([MUTE, timer(20)]);
        true
    }

    /// `0x7d6ec4`: boost (ESS boards), then the PGA step with the request's flags (`+0x90`,
    /// `+0x91`); the trim joins the next unmute.
    fn write_gain(
        &mut self,
        endpoint: GainEndpoint,
        plan: GainPlan,
        verification: Verification,
        effects: &mut Vec<Effect>,
    ) {
        let (control, boost) = match endpoint {
            GainEndpoint::Input1 => (Control::InputGain(Input::One), Boost::Input(Input::One)),
            GainEndpoint::Input2 => (Control::InputGain(Input::Two), Boost::Input(Input::Two)),
            GainEndpoint::Return1 => (Control::ReturnGain(Return::One), Boost::Return(Return::One)),
            GainEndpoint::Return2 => (Control::ReturnGain(Return::Two), Boost::Return(Return::Two)),
        };
        if let Some(enabled) = plan.boost {
            effects.push(Effect::Hal(Command::Boost(boost, i32::from(enabled))));
        }
        effects.push(write(control, Value::Gain(plan.step), verification));
        let slot = slot(endpoint);
        self.cached_step[slot] = Some(plan.step);
        self.trims[slot] = plan.trim;
    }

    fn observe_impedance(&mut self, hal: &mut impl Hal, input: Input) -> Result<(), HalError> {
        if self.cached_z[input as usize].is_none() {
            let control = Control::InputImpedance(input);
            let Value::Impedance(value) = observe(hal, control)? else {
                return Err(HalError::NoReading(Query::Setting(control)));
            };
            self.cached_z[input as usize] = Some(value);
        }
        Ok(())
    }

    /// `0x7d7378`: clears `+0xd`, then the trims (zeros while the external mute is on).
    fn unmute(&mut self) -> Effect {
        self.step = Step::Idle;
        self.muted = false;
        self.trims()
    }

    /// `0x7d6440` / `0x7d7378`: the saved trims to selectors 1, 2, 4, 5 unless either mute
    /// is on.
    fn trims(&self) -> Effect {
        Effect::Dsp(DspCommand::InputTrims(
            (!self.muted && !self.external_mute).then_some(self.trims),
        ))
    }
}

/// HAL cache first, then a read (`ZenHAL` `_cache` slots).
fn observe(hal: &mut impl Hal, control: Control) -> Result<Value, HalError> {
    if let Some(value) = hal.cached(control) {
        return Ok(value);
    }
    match hal.read(Query::Setting(control))? {
        Reading::Value(value) => Ok(value),
        _ => Err(HalError::NoReading(Query::Setting(control))),
    }
}

/// Trim slot: stock endpoint ids 1, 2, 4, 5 → 0..=3 (`0x7d6688`).
fn slot(endpoint: GainEndpoint) -> usize {
    match endpoint {
        GainEndpoint::Input1 => 0,
        GainEndpoint::Input2 => 1,
        GainEndpoint::Return1 => 2,
        GainEndpoint::Return2 => 3,
    }
}

fn write(control: Control, value: Value, verification: Verification) -> Effect {
    let setting = Setting::new(control, value).expect("scheduler uses matching control and value");
    Effect::Hal(Command::Set(setting, verification))
}

fn timer(millis: u64) -> Effect {
    Effect::StartTimer {
        timer: Timer::InputScheduler,
        after: Duration::from_millis(millis),
    }
}

/// Stock single-shot save after the last I/O edit (`+0x148`, `setInterval(250)` at
/// `0x35c95c` → `saveConnectorsValues` `0x353fa0`).
pub const SAVE_DELAY: Duration = Duration::from_millis(250);
/// External mute, then 10 ms before the writes: boot restore (`0x35e588`) and I/O preset
/// load (`0x36352c`).
const RESTORE_SETTLE: Duration = Duration::from_millis(10);
/// Boot restore: a full `usleep(100000)` after the apply loop, before the unmute (`0x35ea70`
/// measures from the last apply, so it always sleeps; `0x35f740`).
const RESTORE_MUTE: Duration = Duration::from_millis(100);
/// Non-ESS QC type write outside the restore: mute, 10 ms, `SetInputType`, 100 ms, unmute
/// (§2.2).
const DIRECT_TYPE_SETTLE: [Duration; 2] = [Duration::from_millis(10), Duration::from_millis(100)];

/// The live I/O SETTINGS values (the stock page widgets), normalized like the file. Edits
/// apply at once; the stored copy follows [`SAVE_DELAY`] after the last one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Connectors {
    hardware: Hardware,
    values: BTreeMap<ConnectorId, f32>,
}

impl Connectors {
    /// Every parameter of the hardware: the stored value or its default (`0x35d874..`).
    pub fn load(settings: &Settings, hardware: Hardware) -> Self {
        let values = ConnectorId::table(hardware)
            .map(|id| (id, settings.io.value(id, hardware)))
            .collect();
        Self { hardware, values }
    }

    pub fn hardware(&self) -> Hardware {
        self.hardware
    }

    /// The live value; 0 for a parameter the hardware does not have.
    pub fn value(&self, id: ConnectorId) -> f32 {
        self.values.get(&id).copied().unwrap_or(0.0)
    }

    /// The option index of a live toggle or choice.
    pub fn index(&self, id: ConnectorId) -> Option<usize> {
        id.spec(self.hardware)?.index(self.value(id))
    }

    fn on(&self, id: ConnectorId) -> bool {
        self.index(id) == Some(1)
    }

    /// `0x3594fc` TS branch on a non-ESS QC: the PHANTOM widget is set to 0 with no apply
    /// (`0x359598`–`0x3595cc`), so a later TYPE → Mic never turns 48 V on by itself.
    fn drop_ts_phantom(&mut self, platform: Platform) {
        if platform.has_ess_codec() {
            return;
        }
        for (kind, phantom) in [
            (ConnectorId::In1Type, ConnectorId::In1Phantom),
            (ConnectorId::In2Type, ConnectorId::In2Phantom),
        ] {
            if self.values.get(&kind).is_some_and(|&value| value < 0.5) {
                self.values.insert(phantom, 0.0);
            }
        }
    }
}

/// OUT 1/2 (XLR) or OUT 3/4: a stereo pair or two mono jacks (`output_0_paired`,
/// `output_1_paired`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputPair {
    Out12,
    Out34,
}

impl OutputPair {
    fn paired(self, settings: &Settings) -> bool {
        match self {
            Self::Out12 => settings.io.out12_paired,
            Self::Out34 => settings.io.out34_paired,
        }
    }

    /// Level, mute and DSP `0x06` output of each mono jack.
    fn jacks(self) -> [(ConnectorId, ConnectorId, u8); 2] {
        use ConnectorId::*;
        match self {
            Self::Out12 => [(Out1Level, Out1Mute, 0), (Out2Level, Out2Mute, 1)],
            Self::Out34 => [(Out3Level, Out3Mute, 2), (Out4Level, Out4Mute, 3)],
        }
    }

    /// The pair's level and mute.
    fn pair(self) -> (ConnectorId, ConnectorId) {
        match self {
            Self::Out12 => (ConnectorId::Out12Level, ConnectorId::Out12Mute),
            Self::Out34 => (ConnectorId::Out34Level, ConnectorId::Out34Mute),
        }
    }

    fn of(id: ConnectorId) -> Option<Self> {
        [Self::Out12, Self::Out34].into_iter().find(|pair| {
            let (level, mute) = pair.pair();
            id == level
                || id == mute
                || pair
                    .jacks()
                    .iter()
                    .any(|&(jack, jack_mute, _)| id == jack || id == jack_mute)
        })
    }
}

/// An I/O edit from the UI or Cortex Control: store the live value, apply it at once, send
/// the one-port Control echo and restart the save timer (`0x35b8e8`, `0x354c74`). OUT 1/2
/// and MULTI OUT headphone feeds mirror each other (`0x35476c`).
pub fn set(state: &mut State, id: ConnectorId, value: f32) -> Result<Vec<Effect>, ZenjackError> {
    if !value.is_finite() || id.spec(state.io.hardware).is_none() {
        return Err(ZenjackError::InvalidValue);
    }
    // `0x35f75c`: a non-ESS QC refuses PHANTOM while TYPE is TS (the UI dims the widget).
    let type_id = match id {
        ConnectorId::In1Phantom => Some(ConnectorId::In1Type),
        ConnectorId::In2Phantom => Some(ConnectorId::In2Type),
        _ => None,
    };
    if !state.platform.has_ess_codec()
        && type_id.is_some_and(|kind| state.io.values.get(&kind).is_some_and(|&v| v < 0.5))
    {
        return Ok(Vec::new());
    }
    let value = value.clamp(0.0, 1.0);
    state.io.values.insert(id, value);
    state.io.drop_ts_phantom(state.platform);
    let twin = match id {
        ConnectorId::HpFeedOut12 => Some(ConnectorId::HpFeedMultiOut),
        ConnectorId::HpFeedMultiOut => Some(ConnectorId::HpFeedOut12),
        _ => None,
    };
    if let Some(twin) = twin {
        state.io.values.insert(twin, value);
    }
    let mut effects = apply(state, id, false, Verification::Unverified)?;
    effects.extend(super::control::io::echo(state, id));
    effects.push(Effect::StartTimer {
        timer: Timer::ConnectorsSave,
        after: SAVE_DELAY,
    });
    Ok(effects)
}

/// `0x3608e8`: a pair change writes its key and re-applies the pair's levels, mutes and
/// (OUT 1/2) ground lifts. An unchanged pair does nothing.
pub fn set_pair(state: &mut State, pair: OutputPair, linked: bool) -> Vec<Effect> {
    if pair.paired(state.settings.settings()) == linked {
        return Vec::new();
    }
    let mut effects = super::actions::update_settings(state, |settings| match pair {
        OutputPair::Out12 => settings.io.out12_paired = linked,
        OutputPair::Out34 => settings.io.out34_paired = linked,
    });
    effects.extend(output_gains(state, pair, None));
    if pair == OutputPair::Out12 {
        effects.extend(xlr_lifts(state, None, Verification::Unverified));
    }
    effects.push(super::control::io::pair_echo(pair, linked));
    effects
}

/// Boot restore (`0x35c4ac`): read every value, external mute on, 10 ms, apply in table
/// order, 100 ms, unmute, then save-all. Gain, impedance and ESS type changes queue in the
/// scheduler and run after it. While restoring (`+0x2ac`, `0x35e57c`..`0x35eab8`) a non-ESS
/// type write skips its own mute wrapper. A refused parameter is a notice.
/// `verification`: the boot restore applies with readback and commgap on
/// ([`ZENUI_VERIFY`](tetrad_hal::zenjack::ZENUI_VERIFY): `0x35476c(v, id, 1, 1)` at `0x35e6d4`, `0x35b068` at `0x35e990`). The
/// settings reload uses this sequence in place of stock `0x360da8`, so it passes the reload's
/// flags (unverified).
pub fn restore(state: &mut State, verification: Verification) -> Vec<Effect> {
    load(state);
    let mut effects = vec![
        Effect::ScheduleInput(InputChange::ExternalMute(true)),
        Effect::Sleep(RESTORE_SETTLE),
    ];
    effects.extend(apply_all(state, true, verification));
    effects.push(Effect::Sleep(RESTORE_MUTE));
    effects.push(Effect::ScheduleInput(InputChange::ExternalMute(false)));
    effects.extend(save(state));
    effects
}

/// An I/O preset load (`0x3634e8`) that waits for the input mute to settle.
#[derive(Debug, Clone, PartialEq)]
pub enum IoPresetLoad {
    /// A stored preset (`super::global_eq::load_io_preset`).
    Preset(super::global_eq::IoPreset),
    /// The factory default (`0x3615a0` at `0x3636cc`): every connector row goes to its
    /// catalog default, then [`reset`].
    Factory,
}

/// Starts an I/O preset load (`0x3634e8`): the input external mute goes on
/// (`0x511904(io, 1)` at `0x363524`), and the load waits `RESTORE_SETTLE` (stock
/// `usleep(10000)` at `0x36352c`), so the trims are zero before the first switch. Tetrad waits
/// on [`Timer::IoPresetSettle`]; a load that comes in that time joins the queue. While a load
/// is queued, `+0x2ac` holds: a non-ESS TYPE edit skips its own mute wrapper, whose unmute
/// would release the load's mute early (stock blocks the main thread instead).
pub fn begin_io_load(state: &mut State, load: IoPresetLoad) -> Vec<Effect> {
    state.io_loads.push(load);
    if state.io_loads.len() > 1 {
        return Vec::new();
    }
    vec![
        Effect::ScheduleInput(InputChange::ExternalMute(true)),
        Effect::StartTimer {
            timer: Timer::IoPresetSettle,
            after: RESTORE_SETTLE,
        },
    ]
}

/// [`Timer::IoPresetSettle`]: every queued load in order, then the mute releases
/// (`0x511904(io, 0)` at `0x36369c`). The loads run with `+0x2ac` set (`reload`,
/// `reset(state, true)`).
pub fn settle_io_loads(state: &mut State) -> Vec<Effect> {
    let mut effects = Vec::new();
    for load in std::mem::take(&mut state.io_loads) {
        match load {
            IoPresetLoad::Preset(preset) => {
                effects.extend(super::global_eq::load_io_preset(state, preset));
            }
            IoPresetLoad::Factory => {
                effects.extend(super::actions::update_settings(state, |settings| {
                    settings.io.params.clear()
                }));
                effects.extend(reset(state, true));
            }
        }
    }
    effects.extend([
        Effect::ScheduleInput(InputChange::ExternalMute(false)),
        Effect::Present,
    ]);
    effects
}

/// The I/O reset (`0x3615a0`) after a settings reset cleared `ConnectorsSettings`, or for the
/// factory default I/O preset: both pairs link (`+0x1b4`, `+0x1b5`) and re-apply (`0x3608e8`),
/// the defaults go live, every connector applies its default in table order, then save-all,
/// so a pending [`Timer::ConnectorsSave`] cannot write the old values back. Last, both EXP
/// calibrations go to 0..255 (`0x320fdc(id, 0, 0xff)`) and Cortex Control gets the full
/// IOSettings push (`0x26ebc0`, [`super::control::io::push`]). `restoring`: the caller
/// holds the external mute and `+0x2ac` (the factory I/O preset, `0x363514`..`0x3636a8`),
/// so a non-ESS type write skips its own mute wrapper, whose unmute would end the hold.
pub fn reset(state: &mut State, restoring: bool) -> Vec<Effect> {
    let mut effects = super::actions::update_settings(state, |settings| {
        settings.io.out12_paired = true;
        settings.io.out34_paired = true;
    });
    load(state);
    effects.extend(output_gains(state, OutputPair::Out12, None));
    effects.extend(output_gains(state, OutputPair::Out34, None));
    effects.extend(xlr_lifts(state, None, Verification::Unverified));
    effects.extend(apply_all(state, restoring, Verification::Unverified));
    effects.extend(save(state));
    effects.extend(super::actions::update_settings(state, |settings| {
        settings.expression = Default::default();
    }));
    effects.push(super::control::io::push(state));
    effects
}

/// The EXP calibration rows of an I/O preset: the MIN and MAX parameter of model 31000 per
/// pedal (map `0x1548748`: EXP 1 → 48, 49; EXP 2 → 50, 51).
const PRESET_CALIBRATION: [[i64; 2]; 2] = [[48, 49], [50, 51]];

/// After an I/O preset load and its reload (`0x3634e8`): each pedal takes the preset's MIN and
/// MAX as integers (`QVariant::toInt` of scene 0, then `0x320fdc(id, min, max)`). The rows hold
/// the raw ADC counts (`0x361c3c`). A pedal without both rows keeps its range (I).
pub fn load_calibration(state: &mut State, rows: &[ParamEntry]) -> Vec<Effect> {
    let int = |index: i64| {
        let row = rows.iter().find(|row| row.index == index)?;
        row.value
            .parse::<f32>()
            .ok()
            .map(|value| value.round() as i32)
    };
    let ranges = PRESET_CALIBRATION.map(|[min, max]| int(min).zip(int(max)));
    super::actions::update_settings(state, |settings| {
        for (pedal, range) in settings.expression.pedals.iter_mut().zip(ranges) {
            if let Some((min, max)) = range {
                *pedal = ExpressionRange { min, max };
            }
        }
    })
}

/// The I/O reload (`0x360da8`) after an I/O preset load wrote `ConnectorsSettings`: live
/// values from the file, with MIDI THRU from the MIDI page (`0x3312c8`), every connector in
/// table order through the normal apply (`0x35476c`, `0x35b494`), then both pairs
/// (`0x3608e8`, which always re-applies). No external mute, no sleep and no save-all (unlike
/// [`restore`] and [`reset`]): the caller holds the mute and `+0x2ac` ([`settle_io_loads`],
/// `0x363514`), so a non-ESS type write skips its own mute wrapper. Stock holds
/// the per-port Control echoes (`0x29a1cc(…, 3)`) and pushes the full IOSettings once
/// (`0x26ebc0`, [`super::control::io::push`]).
pub fn reload(state: &mut State) -> Vec<Effect> {
    load(state);
    let thru = ConnectorId::MidiThru;
    if state.io.values.contains_key(&thru) {
        let on = state.settings.settings().midi.thru;
        state.io.values.insert(thru, f32::from(u8::from(on)));
    }
    let mut effects = apply_all(state, true, Verification::Unverified);
    effects.extend(output_gains(state, OutputPair::Out12, None));
    effects.extend(output_gains(state, OutputPair::Out34, None));
    effects.extend(xlr_lifts(state, None, Verification::Unverified));
    effects.push(super::control::io::push(state));
    effects
}

/// The MIDI page changed `MidiSettings/Passthru`: the I/O MIDI THRU row follows, so both
/// pages and the next boot restore agree (owned; stock keeps two copies).
pub fn follow_midi_thru(state: &mut State) -> Vec<Effect> {
    let on = state.settings.settings().midi.thru;
    let id = ConnectorId::MidiThru;
    if id.spec(state.io.hardware).is_none() || state.io.on(id) == on {
        return Vec::new();
    }
    state.io.values.insert(id, f32::from(u8::from(on)));
    let mut effects: Vec<_> = super::control::io::echo(state, id).into_iter().collect();
    effects.push(Effect::StartTimer {
        timer: Timer::ConnectorsSave,
        after: SAVE_DELAY,
    });
    effects
}

/// For a refused 48 V write (bring-up without `--allow-phantom`): PHANTOM of `input` goes to
/// 0 live and in the stored file at once, Cortex Control gets the echo, and the jack type is
/// applied again without 48 V. Nothing for a hardware without that PHANTOM (owned).
pub fn refuse_phantom(state: &mut State, input: Input) -> Vec<Effect> {
    let (phantom, kind) = match input {
        Input::One => (ConnectorId::In1Phantom, ConnectorId::In1Type),
        Input::Two => (ConnectorId::In2Phantom, ConnectorId::In2Type),
    };
    if !state.io.values.contains_key(&phantom) {
        return Vec::new();
    }
    state.io.values.insert(phantom, 0.0);
    let mut effects = super::actions::update_settings(state, |settings| {
        settings.io.params.entry(phantom.id()).or_default().value = Some(0.0);
    });
    effects.extend(super::control::io::echo(state, phantom));
    // The mini has no TYPE: its PHANTOM row carries the type.
    let id = if state.io.values.contains_key(&kind) {
        kind
    } else {
        phantom
    };
    match apply(state, id, false, Verification::Unverified) {
        Ok(applied) => effects.extend(applied),
        Err(error) => effects.push(Effect::Notice(
            Subsystem::Input,
            format!("I/O setting {} not applied: {error}", id.id()),
        )),
    }
    effects
}

/// Live values from the stored ones (`0x35d874..`), with the non-ESS TS rule.
fn load(state: &mut State) {
    state.io = Connectors::load(state.settings.settings(), state.settings.hardware());
    state.io.drop_ts_phantom(state.platform);
}

/// Every parameter in table order (`0x35e5cc`); a refused one is a notice.
fn apply_all(state: &State, restoring: bool, verification: Verification) -> Vec<Effect> {
    let mut effects = Vec::new();
    for id in ConnectorId::table(state.io.hardware) {
        match apply(state, id, restoring, verification) {
            Ok(applied) => effects.extend(applied),
            Err(error) => effects.push(Effect::Notice(
                Subsystem::Input,
                format!("I/O setting {} not applied: {error}", id.id()),
            )),
        }
    }
    effects
}

/// `saveConnectorsValues` (`0x353fa0`): every live value into `ConnectorsSettings`.
pub fn save(state: &mut State) -> Vec<Effect> {
    let values = state.io.values.clone();
    super::actions::update_settings(state, |settings| {
        for (id, value) in values {
            settings.io.params.entry(id.id()).or_default().value = Some(value);
        }
    })
}

/// One parameter's hardware and DSP writes (`0x35476c` levels, `0x35b494` choices).
/// `restoring` is stock `+0x2ac`; `verification` is the caller's `readback, commgap` pair.
fn apply(
    state: &State,
    id: ConnectorId,
    restoring: bool,
    verification: Verification,
) -> Result<Vec<Effect>, ZenjackError> {
    use ConnectorId::*;
    let io = &state.io;
    let platform = state.platform;
    let v = io.value(id);
    let gain = |endpoint| {
        Ok::<_, ZenjackError>(vec![Effect::ScheduleInput(InputChange::Gain {
            endpoint,
            plan: GainPlan::from_unit(platform, v)?,
            verification,
        })])
    };
    // HAL ground-lift status is `index != 1`: lift on is `Lifted` (0).
    let lift = |control| {
        let status = if io.on(id) {
            GroundLift::Lifted
        } else {
            GroundLift::Grounded
        };
        vec![write(control, Value::GroundLift(status), verification)]
    };
    let impedance = |input| {
        Ok::<_, ZenjackError>(vec![Effect::ScheduleInput(InputChange::Impedance {
            input,
            impedance: Impedance::from_unit(v)?,
            verification,
        })])
    };
    let index = io.index(id).unwrap_or(0);
    Ok(match id {
        In1Level => gain(GainEndpoint::Input1)?,
        In2Level => gain(GainEndpoint::Input2)?,
        Ret1Level => gain(GainEndpoint::Return1)?,
        Ret2Level => gain(GainEndpoint::Return2)?,
        In1Impedance => impedance(Input::One)?,
        In2Impedance => impedance(Input::Two)?,
        In1Type => input_type(state, Input::One, restoring, verification),
        In2Type => input_type(state, Input::Two, restoring, verification),
        // `0x354cc8`: PHANTOM alone acts on ESS or when TYPE is Mic.
        In1Phantom | In2Phantom => {
            let (input, kind) = match id {
                In1Phantom => (Input::One, In1Type),
                _ => (Input::Two, In2Type),
            };
            if platform.has_ess_codec() || io.value(kind) >= 0.5 {
                input_type(state, input, restoring, verification)
            } else {
                Vec::new()
            }
        }
        In1GroundLift => lift(Control::InputGroundLift(Input::One)),
        In2GroundLift => lift(Control::InputGroundLift(Input::Two)),
        Ret1GroundLift => lift(Control::ReturnGroundLift(Return::One)),
        Ret2GroundLift => lift(Control::ReturnGroundLift(Return::Two)),
        Send1Level => vec![output(4, output_gain(v))],
        Send2Level => vec![output(5, output_gain(v))],
        Out1Level | Out1Mute | Out2Level | Out2Mute | Out12Level | Out12Mute | Out3Level
        | Out3Mute | Out4Level | Out4Mute | Out34Level | Out34Mute => {
            let pair = OutputPair::of(id).expect("an output level or mute");
            output_gains(state, pair, Some(id))
        }
        Out1GroundLift | Out2GroundLift | Out12GroundLift => {
            xlr_lifts(state, Some(id), verification)
        }
        // `0x7d6500`: ESS boards take DSP `0x58`, the others the HAL volume (mute at 0).
        HpLevel if platform.has_ess_codec() => vec![dsp(Request::SetHeadphoneOutputLevel {
            level_bits: output_gain(v).to_bits(),
        })],
        HpLevel => {
            let volume = if v <= 0.0 {
                HeadphoneVolume::Muted
            } else {
                HeadphoneVolume::from_unit(v)?
            };
            vec![write(
                Control::HeadphoneVolume,
                Value::HeadphoneVolume(volume),
                verification,
            )]
        }
        HpFeedOut1 | HpFeedOut2 | HpFeedOut3 | HpFeedOut4 | HpFeedSend1 | HpFeedSend2
        | HpFeedOut12 | HpFeedMultiOut | HpFeedOut34 | HpFeedSend12 => {
            let mix = match id.hp_feed_port() {
                Some(19) | None => 1,
                Some(port) => port,
            };
            vec![dsp(Request::SetHeadphoneMixLevel {
                mix,
                level_bits: output_gain(v).to_bits(),
            })]
        }
        // `0x330478(midi, on)`: the MIDI setter stores `Passthru` and switches the jack, so
        // the row goes through the MIDI page setting and its bridge (`SetJackMidiMode`).
        MidiThru => vec![Effect::Action(Action::ChangeSetting {
            id: SettingId::MidiThru,
            value: SettingValue::Bool(index != 0),
        })],
        // No apply path found (I: display only).
        Exp1Position | Exp2Position => Vec::new(),
        UsbLevel => vec![dsp(Request::SetUsbReturnGain {
            gain_bits: usb_gain(v).to_bits(),
        })],
        HpSource => vec![dsp(Request::HeadphoneUsbFeedEnable { value: index as u8 })],
        // `0x514d9c`: core 2, then the control core.
        UsbDryWet => [CoreId::Dsp(DspCore::Core2), CoreId::Control]
            .into_iter()
            .map(|destination| {
                dsp(Request::SwitchUsbIns {
                    destination,
                    value_bits: (index as f32).to_bits(),
                })
            })
            .collect(),
    })
}

/// TYPE + PHANTOM as one jack type (`0x3594fc`, `0x7d88ac`). A non-ESS QC writes it at once,
/// under its own external mute unless the restore or an I/O preset load already holds one
/// (`+0x2ac`, `0x3595fc`; a queued load holds it from its mute to its release,
/// [`begin_io_load`]); ESS boards and the mini queue it in the scheduler (`0x7d8878`).
fn input_type(
    state: &State,
    input: Input,
    restoring: bool,
    verification: Verification,
) -> Vec<Effect> {
    let (type_id, phantom) = match input {
        Input::One => (ConnectorId::In1Type, ConnectorId::In1Phantom),
        Input::Two => (ConnectorId::In2Type, ConnectorId::In2Phantom),
    };
    let ess = state.platform.has_ess_codec();
    let kind = match ComboJack::from_values(state.io.value(type_id), state.io.value(phantom), ess) {
        ComboJack::Ts => InputType::Instrument,
        ComboJack::Xlr => InputType::Xlr,
        ComboJack::XlrPhantom => InputType::XlrPhantom,
    };
    if ess {
        return vec![Effect::ScheduleInput(InputChange::Type {
            input,
            kind,
            verification,
        })];
    }
    let write = write(
        Control::InputType(input),
        Value::InputType(kind),
        verification,
    );
    if restoring || !state.io_loads.is_empty() {
        return vec![write];
    }
    vec![
        Effect::ScheduleInput(InputChange::ExternalMute(true)),
        Effect::Sleep(DIRECT_TYPE_SETTLE[0]),
        write,
        Effect::Sleep(DIRECT_TYPE_SETTLE[1]),
        Effect::ScheduleInput(InputChange::ExternalMute(false)),
    ]
}

/// DSP `0x06` for the outputs `changed` drives under the current pairing (`0x35476c`,
/// mute `0x35a828` → `0x3574e8`: gain 0, else the level); `None` sends both outputs.
fn output_gains(state: &State, pair: OutputPair, changed: Option<ConnectorId>) -> Vec<Effect> {
    let paired = pair.paired(state.settings.settings());
    pair.jacks()
        .into_iter()
        .filter_map(|(level, mute, output_index)| {
            let (level, mute) = if paired { pair.pair() } else { (level, mute) };
            if changed.is_some_and(|id| id != level && id != mute) {
                return None;
            }
            let gain = if state.io.on(mute) {
                0.0
            } else {
                output_gain(state.io.value(level))
            };
            Some(output(output_index, gain))
        })
        .collect()
}

/// XLR OUT 1/2 ground lifts under the OUT 1/2 pairing; `None` writes both jacks.
fn xlr_lifts(
    state: &State,
    changed: Option<ConnectorId>,
    verification: Verification,
) -> Vec<Effect> {
    let paired = OutputPair::Out12.paired(state.settings.settings());
    [
        (ConnectorId::Out1GroundLift, XlrOut::One),
        (ConnectorId::Out2GroundLift, XlrOut::Two),
    ]
    .into_iter()
    .filter_map(|(id, jack)| {
        let id = if paired {
            ConnectorId::Out12GroundLift
        } else {
            id
        };
        if changed.is_some_and(|changed| changed != id) {
            return None;
        }
        let status = if state.io.on(id) {
            GroundLift::Lifted
        } else {
            GroundLift::Grounded
        };
        Some(write(
            Control::XlrOutGroundLift(jack),
            Value::GroundLift(status),
            verification,
        ))
    })
    .collect()
}

fn output(output: u8, gain: f32) -> Effect {
    dsp(Request::SetOutputGain {
        output,
        gain_bits: gain.to_bits(),
    })
}

fn dsp(request: Request) -> Effect {
    Effect::Dsp(DspCommand::Request(request))
}
