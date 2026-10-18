//! Owned GlobalTempo / metronome parameter controller.
//!
//! Pure reconstruction of the ZenUI tempo value path (CorOS 4.1.0 ZenUI, SHA-256 `c3c2d0a6…09d7`):
//! receiver gate `0x00109b60`, applier `0x00382540`, rig setter `0x00513410`,
//! `MetronomeDriver::setParameter` `0x00556e3c`, its flush `0x00557050` and the mode-change bulk
//! resend `0x00512c74`. Wire values are normalized `0..=1`; the controller denormalizes them with
//! owner-supplied descriptor ranges and emits `MetronomeParameter` (0x44) and
//! `MetronomeDestination` (0x46) request frames, the 23-float `MetronomeParameterBlock`, and the
//! `syncAllCores` barrier intent as typed [`ResendStep`]s. No transport, DSP, audio or UI
//! execution. Non-finite wire values, initial store values, descriptor spans, driver values and DSP
//! output values are refused here as owned policy while the codecs keep their bits.

use std::time::Duration;

use tetrad_control::codecs::{EnumValue, GlobalTempoMessage, MessageAction, Param, ParamValue};
use tetrad_dsp_protocol::controls::{ControlRequest, MetronomeParameterBlock, RequestFrame};
use thiserror::Error;

/// `enMetronomeParameter` indices 0..=22; the driver vector holds 23 slots (`0x00558ab8`).
pub const PARAM_COUNT: usize = 23;
/// Preset-mode writes cover eight scenes (`0x00513410`).
pub const SCENE_COUNT: usize = 8;
/// `MIN_TEMPO` / `MAX_TEMPO` / `DEFAULT_TEMPO` in `_INIT_402` (`0x0009932c`); the same
/// 40..240 bounds normalize the BPM in `0x0064c870`, and 120 is the driver default.
pub const MIN_BPM: f32 = 40.0;
pub const MAX_BPM: f32 = 240.0;
pub const DEFAULT_BPM: f32 = 120.0;
/// Relative tolerance of the stock change test (`0x0037febc`, `0x00513410`, `0x0064c870`).
const CHANGE_TOLERANCE: f32 = 1e-5;

/// Typed parameter index. Names marked *inferred* come from the recovered value
/// transform, not from a stock string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempoParam {
    /// 0: BPM. Rounded, drives `0x00875a48` (DSP 0x16 + tempo LED colour).
    Bpm,
    /// 1: tempo mode, 0 preset / 1 global. Always stored globally; never sent to the DSP.
    TempoMode,
    /// 2: tempo LED enable, `0.5 <` threshold (`0x0050880c`). Never sent to the DSP.
    TempoLed,
    /// 3: `VOLUME` in dB, −60..9 (catalog 25000). Keeps fractions; sent as `10^(v/20)` to
    /// destination 1.
    LevelDb,
    /// 4: metronome enable. The flush orders it first (off) or last (on).
    Enabled,
    /// 5: `PAN`, −1..1 (catalog 25000). Keeps fractions; sent as `(v + 1) / 2` to destination 1.
    Pan,
    /// 6: time-signature table index 0..=20 (`0x00558ab8`, `0x00494ab0`).
    TimeSignature,
    /// 7: subdivision choice 0..=3 (`0x00381150`); sent floored.
    Subdivision,
    /// 8: `SOUND`, click kit 0..=5 (catalog 25000, `sendAllSounds` `0x00558554`); sent raw.
    Sound,
    /// 9: click destination choice; sent through 0x46 as 19,0,1,2,3 (`0x00557494`).
    Destination,
    /// 10..=22: accent for beat 0..=12 (`0x0037e89c`, `0x003816d4`).
    Accent(Beat),
}

/// Accent beat 0..=12, the thirteen beat widgets behind indices 10..=22. Construction
/// is checked so [`TempoParam::index`] can never leave the 23-slot driver vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Beat(u8);

impl Beat {
    pub const COUNT: usize = PARAM_COUNT - 10;

    /// `None` for beats 13 and above.
    pub const fn new(beat: u8) -> Option<Self> {
        if (beat as usize) < Self::COUNT {
            Some(Self(beat))
        } else {
            None
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TempoParam {
    pub fn from_index(index: u32) -> Option<Self> {
        Some(match index {
            0 => Self::Bpm,
            1 => Self::TempoMode,
            2 => Self::TempoLed,
            3 => Self::LevelDb,
            4 => Self::Enabled,
            5 => Self::Pan,
            6 => Self::TimeSignature,
            7 => Self::Subdivision,
            8 => Self::Sound,
            9 => Self::Destination,
            10..=22 => Self::Accent(Beat((index - 10) as u8)),
            _ => return None,
        })
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Bpm => 0,
            Self::TempoMode => 1,
            Self::TempoLed => 2,
            Self::LevelDb => 3,
            Self::Enabled => 4,
            Self::Pan => 5,
            Self::TimeSignature => 6,
            Self::Subdivision => 7,
            Self::Sound => 8,
            Self::Destination => 9,
            Self::Accent(beat) => 10 + beat.0 as usize,
        }
    }

    /// Receiver `0x00109b60` brackets these indices with the sender guard (`0x0010a7dc`).
    pub const fn guards_echo(self) -> bool {
        matches!(self, Self::Bpm | Self::LevelDb | Self::Pan)
    }

    /// `0x00513410` / `0x0037fda8` round every denormalized value except 3 and 5.
    const fn keeps_fraction(self) -> bool {
        matches!(self, Self::LevelDb | Self::Pan)
    }

    /// `setParameter` `0x00556e3c` returns early for indices 1 and 2.
    const fn reaches_driver(self) -> bool {
        !matches!(self, Self::TempoMode | Self::TempoLed)
    }
}

/// Descriptor min/max/skew read from the tempo block at `index * 0x118` (+0x60, +0x64, +0x98).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamRange {
    pub min: f32,
    pub max: f32,
    pub skew: f32,
}

impl ParamRange {
    /// BPM range, evidence-closed (`0x0009932c`, `0x0064c870`).
    pub const BPM: Self = Self {
        min: MIN_BPM,
        max: MAX_BPM,
        skew: 1.0,
    };
    /// Identity range used by the mode reader `0x0064c828`.
    pub const UNIT: Self = Self {
        min: 0.0,
        max: 1.0,
        skew: 1.0,
    };
    const PAN: Self = Self {
        min: -1.0,
        max: 1.0,
        skew: 1.0,
    };

    /// Owned admission policy: finite endpoints, positive finite skew, and a finite
    /// non-zero span, so [`Self::denormalize`] is finite for every clamped input.
    fn validate(self, index: usize) -> Result<(), TempoError> {
        if ![self.min, self.max, self.skew]
            .iter()
            .all(|v| v.is_finite())
            || self.skew <= 0.0
        {
            return Err(TempoError::Range(index));
        }
        let span = self.max - self.min;
        if !span.is_finite() || span == 0.0 {
            return Err(TempoError::RangeSpan(index));
        }
        Ok(())
    }

    /// `0x0064b8f8` mode 0: clamp to `0..=1`, apply skew, scale into the range.
    pub fn denormalize(self, normalized: f32) -> f32 {
        let unit = normalized.clamp(0.0, 1.0);
        let shaped = if self.skew == 1.0 || unit <= 0.0 {
            unit
        } else {
            (unit.ln() / self.skew).exp()
        };
        self.min + shaped * (self.max - self.min)
    }

    /// `0x0064b8f8` mode 1 for finite input: scale into `0..=1`, apply skew.
    ///
    /// A zero span has no division to make: stock lands on 1.0 for `value >= min` (NaN
    /// and +inf both clamp to 1.0) and 0.0 below it; this helper returns the same
    /// values explicitly, so it never yields NaN for finite input. The controller
    /// refuses zero-span descriptors at construction regardless.
    pub fn normalize(self, value: f32) -> f32 {
        let span = self.max - self.min;
        let unit = if span == 0.0 {
            if value < self.min { 0.0 } else { 1.0 }
        } else {
            ((value - self.min) / span).clamp(0.0, 1.0)
        };
        if self.skew == 1.0 {
            unit
        } else {
            unit.powf(self.skew)
        }
    }
}

/// Time-signature row from `MetronomeDriver` (`0x00558ab8`) and the beat tables at
/// VA `0x00a47948` (beats) and `0x00a478f0` (unit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSignature {
    pub index: u8,
    pub name: &'static str,
    pub beats: u8,
    /// Beat unit: 4 for `x/4`, 8 for `x/8`.
    pub unit: u8,
}

impl TimeSignature {
    const NAMES: [&'static str; 21] = [
        "2/4",
        "3/4",
        "4/4",
        "5/4",
        "6/4",
        "7/4",
        "8/4",
        "9/4",
        "10/4",
        "11/4",
        "12/4",
        "13/4",
        "3/8",
        "6/8",
        "9/8",
        "12/8",
        "5/8 (3+2)",
        "5/8 (2+3)",
        "7/8 (3+2+2)",
        "7/8 (2+3+2)",
        "7/8 (2+2+3)",
    ];
    const BEATS: [u8; 21] = [
        2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 3, 6, 9, 12, 5, 5, 7, 7, 7,
    ];

    /// `None` outside the 21-row table; `0x00494ab0` falls back to four beats there.
    pub fn from_index(index: i32) -> Option<Self> {
        let row = usize::try_from(index)
            .ok()
            .filter(|&i| i < Self::NAMES.len())?;
        Some(Self {
            index: row as u8,
            name: Self::NAMES[row],
            beats: Self::BEATS[row],
            unit: if row < 12 { 4 } else { 8 },
        })
    }

    /// Default accent levels of slots 10..=22 (`_INIT_375` table `0x0154b55c`, stated as its
    /// rule): beat 0 is 2, the first beat of each later group 3, the rest 0. `x/8` rows group
    /// by their name (threes unless named); an `x/4` row is one group.
    pub fn default_accents(self) -> [u8; Beat::COUNT] {
        let groups: &[u8] = match self.name {
            "5/8 (3+2)" => &[3, 2],
            "5/8 (2+3)" => &[2, 3],
            "7/8 (3+2+2)" => &[3, 2, 2],
            "7/8 (2+3+2)" => &[2, 3, 2],
            "7/8 (2+2+3)" => &[2, 2, 3],
            _ if self.unit == 8 => &[3; 4][..usize::from(self.beats / 3)],
            _ => &[],
        };
        let mut levels = [0; Beat::COUNT];
        levels[0] = 2;
        let mut start = 0;
        for &group in groups {
            start += usize::from(group);
            if start < usize::from(self.beats) {
                levels[start] = 3;
            }
        }
        levels
    }

    /// Subdivision labels offered for index 7 (`0x00381150`: rows below 12 use quarters).
    pub const fn subdivisions(self) -> [&'static str; 4] {
        if self.unit == 4 {
            ["1/4", "1/8", "1/8T", "1/16"]
        } else {
            ["1/8", "1/16", "1/16T", "1/32"]
        }
    }
}

/// Value of index 1 as read by `0x0064c828`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempoMode {
    Preset,
    Global,
}

/// Where an update came from. `Remote` is the cortex receiver, which guards indices 0, 3, 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Remote,
    Local,
}

/// Normalized stored values; `None` was never written and always counts as changed.
pub type Store = [Option<f32>; PARAM_COUNT];

#[derive(Debug, Clone, PartialEq)]
pub struct TempoConfig {
    /// Descriptor range per index. `None` refuses updates for that index.
    pub ranges: [Option<ParamRange>; PARAM_COUNT],
    /// Preset.proto `Param.scene_mode` per index (`0x002ea908`): write one scene, not eight.
    pub scene_mode: [bool; PARAM_COUNT],
}

#[derive(Debug, Clone, PartialEq)]
pub struct TempoState {
    /// Global settings block (`+0x90` of `0x006508e8`).
    pub global: Store,
    /// Preset tempo block, one store per scene.
    pub scenes: [Store; SCENE_COUNT],
    pub current_scene: usize,
}

/// Which echo the stock setter triggers after a changed write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Echo {
    None,
    /// `0x0010b2a4`: UPDATE with the global value; see [`TempoController::global_tempo_update`].
    GlobalTempo,
    /// `0x0025f3f0`: preset-mode `TempoProgramData` inside a Grid message (not encoded here).
    GridTempoProgramData,
}

/// Side effect of `0x00381e88` or the setter that needs an owner action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// `0x00875a48`: DSP `TempoChange` 0x16, encoded by `ControlRequest::TempoChange
    /// { bpm }` and sent with [`SendPolicy::POST`]; also the tempo LED colour.
    TempoChange { bpm: i32 },
    /// `0x0050880c`: tempo LED enable flag.
    TempoLed(bool),
    /// `0x00494ab0`: beats follow the new table row.
    TimeSignature(Option<TimeSignature>),
    /// `0x00512c74`: bulk resend after a tempo-mode change. The owner answers with
    /// [`TempoController::plan_resend`].
    ResendAll,
}

/// Stock send arguments `(wait, timeout)` of the comms vtable call (`+0x34`). `wait`
/// asks the sender to block for the DSP acknowledgement up to `timeout`; the owning
/// transport implements the wait. A step with `wait` set must not be published as
/// fire-and-forget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendPolicy {
    pub wait: bool,
    pub timeout: Duration,
}

impl SendPolicy {
    /// `0x00875d48` with wait 0, and the `0x0051965c` single-fragment path.
    pub const POST: Self = Self {
        wait: false,
        timeout: Duration::from_millis(100),
    };
    /// `0x00875d48` with wait 1: the enable-off and enable-restore frames around the block.
    pub const WAIT: Self = Self {
        wait: true,
        timeout: Duration::from_millis(100),
    };
    /// `syncAllCores` `0x008763e8`: one `DiagnosticsRequest` per core, wait 1 / 1000 ms.
    pub const SYNC: Self = Self {
        wait: true,
        timeout: Duration::from_millis(1000),
    };
}

/// One step of the bulk resend `0x00512c74`, in stock order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResendStep {
    /// A 20-byte 0x44 / 0x46 frame through `0x00875d48`.
    Request {
        frame: RequestFrame,
        policy: SendPolicy,
    },
    /// `syncAllCores` `0x008763e8`: a 16-byte `DiagnosticsRequest` (0x0a) header to DSP
    /// cores 0, 1, 2, 3 in that order, each with [`SendPolicy::SYNC`]; stock logs
    /// `[syncAllCores] - failed to sync core %d` and continues. No codec builds these
    /// bytes here; the step is the synchronous barrier intent.
    SyncAllCores,
    /// The 108-byte block of all 23 raw slot floats through `0x0051965c`, [`SendPolicy::POST`].
    Block(MetronomeParameterBlock),
}

/// Result of [`TempoController::plan_resend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResendPlan {
    pub steps: Vec<ResendStep>,
    /// Slot 9 (destination) or 5 (pan) changed: the metronome is switched off and every
    /// core synchronized before the block, the block carries 0.0 for slot 4, and the real
    /// enable value follows a second barrier.
    pub rerouted: bool,
    /// `0x00494ab0(timesig, 0)`: beats follow the re-read index 6; `None` outside the table.
    pub time_signature: Option<TimeSignature>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Applied {
    pub param: TempoParam,
    /// Wire (normalized) value.
    pub value: f32,
    pub changed: bool,
    /// Denormalized value handed to the driver, rounded unless the index keeps fractions.
    pub real: Option<f32>,
    pub echo: Echo,
    /// Stock defers and coalesces the GlobalTempo echo per index while guarded.
    pub guarded: bool,
    pub effect: Option<Effect>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Handled {
    Applied(Applied),
    /// Reply to action READ (`0x0010aa1c`).
    Read(GlobalTempoMessage),
    Ignored,
}

/// The four stock log cases of receiver `0x00109b60`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum Gate {
    #[error("message action UPDATE has no param")]
    NoParam,
    #[error("param has no index")]
    NoIndex,
    #[error("param has no value")]
    NoValue,
    #[error("ParamValue doesn't contain float value")]
    NotFloat,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TempoError {
    #[error("parameter index {0} is outside 0..=22")]
    Index(u32),
    #[error("value bits {bits:#010x} for index {index} are not finite")]
    NonFinite { index: u32, bits: u32 },
    #[error("no descriptor range configured for {0:?}")]
    RangeUnknown(TempoParam),
    #[error("descriptor range for index {0} is not finite or has a non-positive skew")]
    Range(usize),
    #[error("descriptor range for index {0} has a zero or non-representable span")]
    RangeSpan(usize),
    #[error("current scene {0} is outside 0..=7")]
    Scene(usize),
    #[error("initial global value bits {bits:#010x} for index {index} are not finite")]
    GlobalNonFinite { index: usize, bits: u32 },
    #[error("initial scene {scene} value bits {bits:#010x} for index {index} are not finite")]
    SceneNonFinite {
        scene: usize,
        index: usize,
        bits: u32,
    },
    #[error("index {index} denormalizes to bits {real_bits:#010x}, which has no finite DSP value")]
    Unrepresentable { index: u32, real_bits: u32 },
    #[error("tempo mode (index 1) is not set in the global store")]
    ModeUnset,
    #[error("index {0} has no value in the global store; the resend does not guess 0.0")]
    GlobalUnset(usize),
    #[error("index {index} has no value in scene {scene}; the resend does not guess 0.0")]
    SceneUnset { scene: usize, index: usize },
    #[error("driver value bits {bits:#010x} for index {index} are not finite")]
    DriverNonFinite { index: usize, bits: u32 },
    #[error("driver slots are dirty; flush or resend before initializing the driver")]
    DriverDirty,
    #[error("tempo mode value {0} is neither 0 (preset) nor 1 (global)")]
    ModeUnknown(i32),
    #[error("UPDATE gate: {0}")]
    Gate(#[from] Gate),
    #[error(transparent)]
    Codec(#[from] tetrad_dsp_protocol::Error),
}

#[derive(Debug, Clone, Copy, Default)]
struct DriverSlot {
    value: Option<f32>,
    dirty: bool,
}

#[derive(Debug, Clone)]
pub struct TempoController {
    config: TempoConfig,
    state: TempoState,
    driver: [DriverSlot; PARAM_COUNT],
    flush_armed: bool,
}

impl TempoController {
    pub fn new(config: TempoConfig, state: TempoState) -> Result<Self, TempoError> {
        for (index, range) in config.ranges.iter().enumerate() {
            if let Some(range) = range {
                range.validate(index)?;
            }
        }
        if state.current_scene >= SCENE_COUNT {
            return Err(TempoError::Scene(state.current_scene));
        }
        // `None` is the documented unknown state; a non-finite `Some` would defeat the
        // change test forever and leak into READ replies, so it is refused with its location.
        if let Some((index, bits)) = first_non_finite(&state.global) {
            return Err(TempoError::GlobalNonFinite { index, bits });
        }
        for (scene, store) in state.scenes.iter().enumerate() {
            if let Some((index, bits)) = first_non_finite(store) {
                return Err(TempoError::SceneNonFinite { scene, index, bits });
            }
        }
        // `setParameter` returns before touching indices 1 and 2, so those stock slots keep
        // the constructor value 0.0 (`0x00558ab8`) for good and the block copies it. Every
        // other slot is unknown until `apply`, `initialize_driver` or `plan_resend` sets it.
        let mut driver = [DriverSlot::default(); PARAM_COUNT];
        for param in [TempoParam::TempoMode, TempoParam::TempoLed] {
            driver[param.index()].value = Some(0.0);
        }
        Ok(Self {
            config,
            state,
            driver,
            flush_armed: false,
        })
    }

    pub fn state(&self) -> &TempoState {
        &self.state
    }

    /// Last value handed to the driver slot, before the flush transforms.
    pub fn driver_value(&self, param: TempoParam) -> Option<f32> {
        self.driver[param.index()].value
    }

    /// True while a changed driver value waits for [`Self::flush`] (the stock single-shot timer).
    pub fn flush_armed(&self) -> bool {
        self.flush_armed
    }

    /// Declare the values the DSP-side driver mirror already holds, for all 23 slots.
    /// Stock seeds its vector in the constructor (`0x00558ab8`); the owner supplies the
    /// values it can justify instead. Transactional: a non-finite value or a pending dirty
    /// slot refuses before any slot changes. The slots become clean, so the next
    /// [`Self::plan_resend`] compares against these values.
    pub fn initialize_driver(&mut self, values: [f32; PARAM_COUNT]) -> Result<(), TempoError> {
        if let Some((index, value)) = values.iter().enumerate().find(|(_, v)| !v.is_finite()) {
            return Err(TempoError::DriverNonFinite {
                index,
                bits: value.to_bits(),
            });
        }
        if self.driver.iter().any(|slot| slot.dirty) {
            return Err(TempoError::DriverDirty);
        }
        for (slot, value) in self.driver.iter_mut().zip(values) {
            *slot = DriverSlot {
                value: Some(value),
                dirty: false,
            };
        }
        Ok(())
    }

    /// `0x0064c828`: round the identity-denormalized global index 1.
    pub fn mode(&self) -> Result<TempoMode, TempoError> {
        let raw = self.state.global[TempoParam::TempoMode.index()].ok_or(TempoError::ModeUnset)?;
        match ParamRange::UNIT.denormalize(raw).round() as i32 {
            0 => Ok(TempoMode::Preset),
            1 => Ok(TempoMode::Global),
            other => Err(TempoError::ModeUnknown(other)),
        }
    }

    /// Receiver `0x00109b60`: UPDATE applies the first param's first float with the
    /// 0/3/5 guard; READ answers with the global store; other actions are ignored.
    pub fn handle(&mut self, message: &GlobalTempoMessage) -> Result<Handled, TempoError> {
        match message.action {
            Some(EnumValue::Known(MessageAction::Update)) => {
                let (index, value) = gate(message)?;
                self.apply(index, value, Source::Remote)
                    .map(Handled::Applied)
            }
            Some(EnumValue::Known(MessageAction::Read)) => Ok(Handled::Read(self.read_response())),
            _ => Ok(Handled::Ignored),
        }
    }

    /// `0x00382540` → `0x00513410` → `0x00556e3c`: store, denormalize, update the driver slot.
    pub fn apply(&mut self, index: u32, value: f32, source: Source) -> Result<Applied, TempoError> {
        let param = TempoParam::from_index(index).ok_or(TempoError::Index(index))?;
        if !value.is_finite() {
            return Err(TempoError::NonFinite {
                index,
                bits: value.to_bits(),
            });
        }
        let mode = self.mode()?;
        let guarded = source == Source::Remote && param.guards_echo();
        // Refuse before any store or dirty flag changes.
        let real = self.real_value(param, value)?;
        if !self.store(param, value, mode) {
            return Ok(Applied {
                param,
                value,
                changed: false,
                real: None,
                echo: Echo::None,
                guarded,
                effect: None,
            });
        }
        if param.reaches_driver() {
            let slot = &mut self.driver[param.index()];
            slot.dirty |= slot.value != Some(real);
            slot.value = Some(real);
            self.flush_armed |= slot.dirty;
        }
        // The setter re-reads the mode after the write, so index 1 echoes under either mode.
        let echo = match (self.mode()?, param) {
            (TempoMode::Global, _) | (TempoMode::Preset, TempoParam::TempoMode) => {
                Echo::GlobalTempo
            }
            (TempoMode::Preset, _) => Echo::GridTempoProgramData,
        };
        let effect = match param {
            TempoParam::Bpm => Some(Effect::TempoChange { bpm: real as i32 }),
            TempoParam::TempoMode => Some(Effect::ResendAll),
            TempoParam::TempoLed => Some(Effect::TempoLed(0.5 < real)),
            TempoParam::TimeSignature => Some(Effect::TimeSignature(TimeSignature::from_index(
                real as i32,
            ))),
            _ => None,
        };
        Ok(Applied {
            param,
            value,
            changed: true,
            real: Some(real),
            echo,
            guarded,
            effect,
        })
    }

    /// `setParameter` `0x00556e3c` alone, as the MIDI clock BPM uses it: update the driver
    /// slot with a real (denormalized) value and arm the flush, with no store write and no
    /// echo. Indices 1 and 2 return early, as in stock. Returns whether the slot went dirty
    /// (the owner then starts the flush timer). A value the flush cannot carry is refused
    /// before the slot changes.
    pub fn set_driver(&mut self, param: TempoParam, real: f32) -> Result<bool, TempoError> {
        if !param.reaches_driver() {
            return Ok(false);
        }
        dsp_value(param.index(), real)?;
        let slot = &mut self.driver[param.index()];
        let changed = slot.value != Some(real);
        slot.dirty |= changed;
        slot.value = Some(real);
        self.flush_armed |= slot.dirty;
        Ok(changed)
    }

    /// `0x00513410` / `0x002ea184`: denormalize with the descriptor, round unless the index
    /// keeps fractions, and require both the driver value and its flush float to be finite.
    fn real_value(&self, param: TempoParam, value: f32) -> Result<f32, TempoError> {
        let range = self.config.ranges[param.index()].ok_or(TempoError::RangeUnknown(param))?;
        let mut real = range.denormalize(value);
        if !param.keeps_fraction() {
            real = real.round();
        }
        dsp_value(param.index(), real)?;
        Ok(real)
    }

    /// `0x00513410`: index 1 and global mode write the global store; preset mode writes the
    /// current scene, or all eight scenes unless the index is scene-mode. Returns `changed`.
    fn store(&mut self, param: TempoParam, value: f32, mode: TempoMode) -> bool {
        let index = param.index();
        if mode == TempoMode::Global || param == TempoParam::TempoMode {
            if !differs(self.state.global[index], value) {
                return false;
            }
            self.state.global[index] = Some(value);
            return true;
        }
        let current = self.state.current_scene;
        if !differs(self.state.scenes[current][index], value) {
            return false;
        }
        if self.config.scene_mode[index] {
            self.state.scenes[current][index] = Some(value);
        } else {
            for scene in &mut self.state.scenes {
                scene[index] = Some(value);
            }
        }
        true
    }

    /// `parameterChangeTimerSlot` `0x00557050`: emit every dirty slot in index order, with the
    /// enable flag sent first when turning off and last (as 1.0) when turning on.
    ///
    /// Transactional: every frame is built before any dirty flag is cleared, so a refusal
    /// leaves the driver slots and [`Self::flush_armed`] exactly as they were.
    pub fn flush(&mut self) -> Result<Vec<RequestFrame>, TempoError> {
        let mut frames = Vec::new();
        let mut enable_last = false;
        let enable = self.driver[TempoParam::Enabled.index()];
        if let (true, Some(value)) = (enable.dirty, enable.value) {
            if value >= 0.5 {
                enable_last = true;
            } else {
                frames.push(metronome_parameter(4, 4, 0.0)?);
            }
        }
        for (index, slot) in self.driver.iter().enumerate() {
            let (true, Some(value)) = (slot.dirty, slot.value) else {
                continue;
            };
            // The flush loop skips 1, 2 and 4; the enable frame is ordered above and below.
            if !matches!(index, 1 | 2 | 4) {
                frames.push(slot_frame(index, value)?);
            }
        }
        if enable_last {
            frames.push(metronome_parameter(4, 4, 1.0)?);
        }
        for slot in &mut self.driver {
            slot.dirty = false;
        }
        self.flush_armed = false;
        Ok(frames)
    }

    /// `0x00512c74`, run by the owner on [`Effect::ResendAll`]: re-derive every driver slot
    /// from the store the new mode selects (global store in global mode, current scene in
    /// preset mode) with `setParameter(…, mode 1)` semantics (dirty flags only, no timer),
    /// then plan the stock send sequence:
    ///
    /// | Step | When | Frame |
    /// | --- | --- | --- |
    /// | a | slot 9 or slot 5 dirty | 0x44 dest 4 param 4 = 0.0, [`SendPolicy::WAIT`] |
    /// | b | same | [`ResendStep::SyncAllCores`] |
    /// | c | same | 0x46 dest 1 word 9 = mapped destination choice |
    /// | d | same | 0x44 dest 1 param 5 = `(pan + 1) / 2` |
    /// | e | always | 0x44 dest 1 param 3 = `10^(dB / 20)` |
    /// | f | always | block of the 23 raw slot floats; slot 4 is 0.0 when a ran |
    /// | g | a ran | [`ResendStep::SyncAllCores`], then 0x44 dest 4 param 4 = raw slot 4, `WAIT` |
    ///
    /// Dirty is judged against the current driver values, so an unknown driver takes the
    /// full a..g path. Transactional: an unset store value, missing range, unknown mode or
    /// unrepresentable DSP float refuses before any slot, dirty flag or `flush_armed`
    /// changes. On success every slot is clean and no flush waits.
    pub fn plan_resend(&mut self) -> Result<ResendPlan, TempoError> {
        let mode = self.mode()?;
        let mut driver = self.driver;
        for (index, slot) in driver.iter_mut().enumerate() {
            let param =
                TempoParam::from_index(index as u32).ok_or(TempoError::Index(index as u32))?;
            if !param.reaches_driver() {
                continue;
            }
            let stored = if mode == TempoMode::Global {
                self.state.global[index].ok_or(TempoError::GlobalUnset(index))?
            } else {
                let scene = self.state.current_scene;
                self.state.scenes[scene][index].ok_or(TempoError::SceneUnset { scene, index })?
            };
            let real = self.real_value(param, stored)?;
            slot.dirty |= slot.value != Some(real);
            slot.value = Some(real);
        }
        // Slots 1 and 2 are seeded in `new`; the loop above set every other slot.
        let raw = |param: TempoParam| {
            driver[param.index()]
                .value
                .expect("every driver slot is set after the resend loop")
        };
        let rerouted =
            driver[TempoParam::Destination.index()].dirty || driver[TempoParam::Pan.index()].dirty;
        let request = |frame, policy| ResendStep::Request { frame, policy };
        let mut steps = Vec::with_capacity(if rerouted { 8 } else { 2 });
        if rerouted {
            steps.push(request(metronome_parameter(4, 4, 0.0)?, SendPolicy::WAIT));
            steps.push(ResendStep::SyncAllCores);
            steps.push(request(
                slot_frame(9, raw(TempoParam::Destination))?,
                SendPolicy::POST,
            ));
            steps.push(request(
                slot_frame(5, raw(TempoParam::Pan))?,
                SendPolicy::POST,
            ));
        }
        steps.push(request(
            slot_frame(3, raw(TempoParam::LevelDb))?,
            SendPolicy::POST,
        ));
        let mut bits = [0u32; PARAM_COUNT];
        for (bits, slot) in bits.iter_mut().zip(&driver) {
            *bits = slot
                .value
                .expect("every driver slot is set after the resend loop")
                .to_bits();
        }
        if rerouted {
            bits[TempoParam::Enabled.index()] = 0f32.to_bits();
        }
        steps.push(ResendStep::Block(MetronomeParameterBlock::new(bits)));
        if rerouted {
            steps.push(ResendStep::SyncAllCores);
            steps.push(request(
                metronome_parameter(4, 4, raw(TempoParam::Enabled))?,
                SendPolicy::WAIT,
            ));
        }
        let time_signature = TimeSignature::from_index(raw(TempoParam::TimeSignature) as i32);
        // Commit: stock clears 9, 5 and 3 as it sends them and the block copy clears the rest.
        for slot in &mut driver {
            slot.dirty = false;
        }
        self.driver = driver;
        self.flush_armed = false;
        Ok(ResendPlan {
            steps,
            rerouted,
            time_signature,
        })
    }

    /// `0x0010b2a4`: UPDATE carrying the global store value of `param`, or `None` if unset.
    pub fn global_tempo_update(&self, param: TempoParam) -> Option<GlobalTempoMessage> {
        let value = self.state.global[param.index()]?;
        Some(update_message(vec![indexed_float(param.index(), value)]))
    }

    /// `0x0010aa1c`: UPDATE with every set global value in index order.
    pub fn read_response(&self) -> GlobalTempoMessage {
        let params = self
            .state
            .global
            .iter()
            .enumerate()
            .filter_map(|(index, value)| value.map(|value| indexed_float(index, value)))
            .collect();
        update_message(params)
    }
}

/// Stock test: `min(|current|, |new|) * 1e-5 < |current - new|`.
fn differs(current: Option<f32>, new: f32) -> bool {
    match current {
        None => true,
        Some(current) => current.abs().min(new.abs()) * CHANGE_TOLERANCE < (current - new).abs(),
    }
}

fn gate(message: &GlobalTempoMessage) -> Result<(u32, f32), Gate> {
    let param = message.params.first().ok_or(Gate::NoParam)?;
    let index = param.index.ok_or(Gate::NoIndex)?;
    let value = param.first_value().ok_or(Gate::NoValue)?;
    Ok((index, value.as_float().ok_or(Gate::NotFloat)?))
}

/// First non-finite stored value as `(index, bits)`; `None` entries are the unknown state.
fn first_non_finite(store: &Store) -> Option<(usize, u32)> {
    store
        .iter()
        .enumerate()
        .find_map(|(index, value)| match value {
            Some(value) if !value.is_finite() => Some((index, value.to_bits())),
            _ => None,
        })
}

/// Float the flush (`0x00557050`) carries for a driver value: `10^(v/20)` for 3,
/// `(v+1)/2` for 5, `floor` for 7, the 0x46 choice map for 9, raw otherwise. Owned
/// policy above the bit-preserving codec: both the driver value and the result must be
/// finite, which only index 3 can break (dB above roughly 770 overflows `f32`).
fn dsp_value(index: usize, real: f32) -> Result<f32, TempoError> {
    let value = match index {
        3 => 10f32.powf(real / 20.0),
        5 => ParamRange::PAN.normalize(real),
        7 => real.floor(),
        9 => destination_value(real),
        _ => real,
    };
    if real.is_finite() && value.is_finite() {
        Ok(value)
    } else {
        Err(TempoError::Unrepresentable {
            index: index as u32,
            real_bits: real.to_bits(),
        })
    }
}

/// Single-slot frame of `0x00557050` and `0x00875d48`: destination 1 for 3 and 5, the
/// 0x46 word-9 frame for 9, destination 4 otherwise. Callers never pass 1, 2 or 4.
fn slot_frame(index: usize, real: f32) -> Result<RequestFrame, TempoError> {
    let value = dsp_value(index, real)?;
    Ok(match index {
        3 | 5 => metronome_parameter(1, index as u32, value)?,
        9 => RequestFrame::new(ControlRequest::MetronomeDestination {
            value_bits: value.to_bits(),
        })?,
        _ => metronome_parameter(4, index as u32, value)?,
    })
}

fn metronome_parameter(
    destination: u8,
    parameter: u32,
    value: f32,
) -> Result<RequestFrame, TempoError> {
    Ok(RequestFrame::new(ControlRequest::MetronomeParameter {
        destination,
        parameter,
        value_bits: value.to_bits(),
    })?)
}

/// `0x00557494`: floor, then map choices 0..=4 to 19, 0, 1, 2, 3; others pass through.
fn destination_value(value: f32) -> f32 {
    let choice = value.floor() as i32;
    let mapped = match choice {
        0 => 19,
        1..=4 => choice - 1,
        other => other,
    };
    mapped as f32
}

fn indexed_float(index: usize, value: f32) -> Param {
    Param {
        index: Some(index as u32),
        param_values: vec![ParamValue::float(value)],
        ..Param::default()
    }
}

fn update_message(params: Vec<Param>) -> GlobalTempoMessage {
    GlobalTempoMessage {
        action: Some(MessageAction::Update.into()),
        params,
        ..GlobalTempoMessage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_ranges() -> [Option<ParamRange>; PARAM_COUNT] {
        let mut ranges = [Some(ParamRange::UNIT); PARAM_COUNT];
        ranges[0] = Some(ParamRange::BPM);
        ranges[3] = Some(ParamRange {
            min: -40.0,
            max: 6.0,
            skew: 1.0,
        });
        ranges[5] = Some(ParamRange::PAN);
        ranges[6] = Some(ParamRange {
            min: 0.0,
            max: 20.0,
            skew: 1.0,
        });
        ranges[9] = Some(ParamRange {
            min: 0.0,
            max: 4.0,
            skew: 1.0,
        });
        ranges
    }

    fn controller(mode: f32) -> TempoController {
        let mut global = [None; PARAM_COUNT];
        global[1] = Some(mode);
        TempoController::new(
            TempoConfig {
                ranges: unit_ranges(),
                scene_mode: [false; PARAM_COUNT],
            },
            TempoState {
                global,
                scenes: [[None; PARAM_COUNT]; SCENE_COUNT],
                current_scene: 2,
            },
        )
        .unwrap()
    }

    fn frame_bytes(frames: &[RequestFrame]) -> Vec<Vec<u8>> {
        frames.iter().map(|f| f.as_bytes().to_vec()).collect()
    }

    fn parameter_bytes(destination: u8, parameter: u8, value: f32) -> Vec<u8> {
        let mut bytes = vec![
            0x44,
            destination,
            0,
            0,
            parameter,
            0,
            0,
            0,
            0x14,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        bytes.extend_from_slice(&value.to_bits().to_le_bytes());
        bytes
    }

    #[test]
    fn remote_bpm_update_is_guarded_and_flushes_one_literal_frame() {
        let mut tempo = controller(1.0);
        let applied = tempo.apply(0, 0.4, Source::Remote).unwrap();
        assert_eq!(applied.real, Some(120.0));
        assert_eq!(applied.effect, Some(Effect::TempoChange { bpm: 120 }));
        assert_eq!(applied.echo, Echo::GlobalTempo);
        assert!(applied.guarded && tempo.flush_armed());
        assert_eq!(tempo.state().global[0], Some(0.4));
        let echo = tempo.global_tempo_update(TempoParam::Bpm).unwrap();
        assert_eq!(echo.first_indexed_float(), Some((0, 0.4)));
        assert_eq!(echo.action, Some(MessageAction::Update.into()));

        assert_eq!(
            frame_bytes(&tempo.flush().unwrap()),
            vec![parameter_bytes(4, 0, 120.0)]
        );
        assert!(!tempo.flush_armed() && tempo.flush().unwrap().is_empty());
        // Local source is never guarded; an unchanged value stays inert.
        let again = tempo.apply(0, 0.4, Source::Local).unwrap();
        assert!(!again.changed && !again.guarded && !tempo.flush_armed());
    }

    #[test]
    fn preset_mode_tolerance_and_scene_writes() {
        let mut tempo = controller(0.0);
        tempo.config.scene_mode[11] = true;
        let applied = tempo.apply(10, 0.5, Source::Local).unwrap();
        assert_eq!(
            (applied.real, applied.echo),
            (Some(1.0), Echo::GridTempoProgramData)
        );
        assert!(tempo.state().scenes.iter().all(|s| s[10] == Some(0.5)));
        assert!(tempo.state().global[10].is_none());
        // Within 1e-5 relative: ignored; stored value and driver slot keep the old value.
        let near = tempo.apply(10, 0.5 + 0.5 * 4e-6, Source::Local).unwrap();
        assert!(!near.changed);
        assert_eq!(tempo.state().scenes[2][10], Some(0.5));
        // Scene-mode index writes only the current scene.
        tempo.apply(11, 1.0, Source::Local).unwrap();
        let scenes = &tempo.state().scenes;
        assert_eq!(scenes[2][11], Some(1.0));
        assert!(
            scenes
                .iter()
                .enumerate()
                .all(|(i, s)| i == 2 || s[11].is_none())
        );
        // `None` is always a change even for zero, unlike the stock 0.0 default.
        assert!(tempo.apply(12, 0.0, Source::Local).unwrap().changed);
    }

    #[test]
    fn flush_orders_enable_and_applies_transforms() {
        let mut tempo = controller(1.0);
        for (index, value) in [
            (4, 1.0),
            (3, 0.73913),
            (5, 0.5),
            (7, 0.9),
            (9, 0.0),
            (0, 1.0),
        ] {
            tempo.apply(index, value, Source::Local).unwrap();
        }
        let level_db = tempo.driver_value(TempoParam::LevelDb).unwrap();
        assert!((level_db + 6.0).abs() < 1e-4, "{level_db}");
        let frames = frame_bytes(&tempo.flush().unwrap());
        let mut destination = vec![0x46, 1, 0, 0, 9, 0, 0, 0, 0x14, 0, 0, 0, 0, 0, 0, 0];
        destination.extend_from_slice(&19f32.to_le_bytes());
        assert_eq!(
            frames,
            vec![
                parameter_bytes(4, 0, 240.0),
                parameter_bytes(1, 3, 10f32.powf(level_db / 20.0)),
                parameter_bytes(1, 5, 0.5),
                parameter_bytes(4, 7, 1.0),
                destination,
                parameter_bytes(4, 4, 1.0),
            ]
        );
        // Turning off goes first, before any other dirty slot.
        tempo.apply(4, 0.0, Source::Local).unwrap();
        tempo.apply(9, 0.75, Source::Local).unwrap();
        let mut destination = vec![0x46, 1, 0, 0, 9, 0, 0, 0, 0x14, 0, 0, 0, 0, 0, 0, 0];
        destination.extend_from_slice(&2f32.to_le_bytes());
        assert_eq!(
            frame_bytes(&tempo.flush().unwrap()),
            vec![parameter_bytes(4, 4, 0.0), destination]
        );
    }

    #[test]
    fn tempo_mode_and_led_never_reach_the_driver() {
        let mut tempo = controller(0.0);
        let switched = tempo.apply(1, 1.0, Source::Remote).unwrap();
        assert_eq!(
            (switched.echo, switched.effect),
            (Echo::GlobalTempo, Some(Effect::ResendAll))
        );
        assert_eq!(tempo.mode().unwrap(), TempoMode::Global);
        assert!(!switched.guarded && !tempo.flush_armed());
        let led = tempo.apply(2, 1.0, Source::Remote).unwrap();
        assert_eq!(led.effect, Some(Effect::TempoLed(true)));
        assert!(!led.guarded && !tempo.flush_armed());
        assert!(tempo.flush().unwrap().is_empty());
        assert_eq!(tempo.read_response().first_indexed_float(), Some((1, 1.0)));
        assert_eq!(tempo.read_response().params.len(), 2);
    }

    #[test]
    fn gate_and_refusals_keep_state_inert() {
        let mut tempo = controller(1.0);
        let mut message = update_message(vec![]);
        assert_eq!(tempo.handle(&message), Err(TempoError::Gate(Gate::NoParam)));
        message.params.push(Param {
            index: Some(0),
            param_values: vec![ParamValue::int(120)],
            ..Param::default()
        });
        assert_eq!(
            tempo.handle(&message),
            Err(TempoError::Gate(Gate::NotFloat))
        );
        message.params[0].param_values = vec![ParamValue::float(f32::from_bits(0x7fc0_0001))];
        assert_eq!(
            tempo.handle(&message),
            Err(TempoError::NonFinite {
                index: 0,
                bits: 0x7fc0_0001
            })
        );
        message.params[0].index = Some(23);
        assert_eq!(tempo.handle(&message), Err(TempoError::Index(23)));
        message.params[0].param_values = vec![ParamValue::float(0.5)];
        tempo.config.ranges[8] = None;
        message.params[0].index = Some(8);
        assert_eq!(
            tempo.handle(&message),
            Err(TempoError::RangeUnknown(TempoParam::Sound))
        );
        assert!(tempo.state().global.iter().skip(2).all(Option::is_none));
        assert!(!tempo.flush_armed());

        message.action = Some(MessageAction::Delete.into());
        assert_eq!(tempo.handle(&message), Ok(Handled::Ignored));
        message.action = Some(MessageAction::Read.into());
        let Ok(Handled::Read(reply)) = tempo.handle(&message) else {
            panic!("READ must answer");
        };
        assert_eq!(reply.first_indexed_float(), Some((1, 1.0)));
        let mut unset = controller(1.0);
        unset.state.global[1] = None;
        assert_eq!(
            unset.apply(0, 0.5, Source::Local),
            Err(TempoError::ModeUnset)
        );
    }

    #[test]
    fn time_signature_table_rows() {
        let mut tempo = controller(1.0);
        let applied = tempo.apply(6, 0.1, Source::Local).unwrap();
        let four_four = TimeSignature::from_index(2).unwrap();
        assert_eq!(applied.effect, Some(Effect::TimeSignature(Some(four_four))));
        assert_eq!(
            (four_four.name, four_four.beats, four_four.unit),
            ("4/4", 4, 4)
        );
        assert_eq!(four_four.subdivisions(), ["1/4", "1/8", "1/8T", "1/16"]);
        let three_eight = TimeSignature::from_index(12).unwrap();
        assert_eq!(
            (three_eight.name, three_eight.beats, three_eight.unit),
            ("3/8", 3, 8)
        );
        assert_eq!(three_eight.subdivisions(), ["1/8", "1/16", "1/16T", "1/32"]);
        let last = TimeSignature::from_index(20).unwrap();
        assert_eq!((last.name, last.beats), ("7/8 (2+2+3)", 7));
        assert_eq!(TimeSignature::from_index(21), None);
        assert_eq!(TimeSignature::from_index(-1), None);
    }

    /// `0x0154b55c` rule: 2 on beat 0, 3 on each later group start, 0 elsewhere and past the
    /// last beat.
    #[test]
    fn default_accents_follow_the_grouping() {
        let accents = |index| TimeSignature::from_index(index).unwrap().default_accents();
        assert_eq!(accents(2), [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(accents(13), [2, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(accents(15), [2, 0, 0, 3, 0, 0, 3, 0, 0, 3, 0, 0, 0]);
        assert_eq!(accents(17), [2, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(accents(18), [2, 0, 0, 3, 0, 3, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn ranges_follow_the_stock_mapping() {
        assert_eq!(ParamRange::BPM.denormalize(1.5), MAX_BPM);
        assert_eq!(ParamRange::BPM.denormalize(-1.0), MIN_BPM);
        assert_eq!(ParamRange::BPM.normalize(DEFAULT_BPM), 0.4);
        let skewed = ParamRange {
            min: 0.0,
            max: 100.0,
            skew: 2.0,
        };
        assert!((skewed.denormalize(0.25) - 50.0).abs() < 1e-4);
        assert_eq!(skewed.denormalize(0.0), 0.0);
        assert_eq!(
            TempoController::new(
                TempoConfig {
                    ranges: [Some(ParamRange {
                        min: 0.0,
                        max: 1.0,
                        skew: 0.0
                    }); PARAM_COUNT],
                    scene_mode: [false; PARAM_COUNT],
                },
                TempoState {
                    global: [None; PARAM_COUNT],
                    scenes: [[None; PARAM_COUNT]; SCENE_COUNT],
                    current_scene: 0,
                },
            )
            .err(),
            Some(TempoError::Range(0))
        );
    }

    #[test]
    fn accent_beats_are_bounded() {
        let last = Beat::new(12).unwrap();
        assert_eq!((last.get(), Beat::COUNT, Beat::new(13)), (12, 13, None));
        assert_eq!(TempoParam::from_index(22), Some(TempoParam::Accent(last)));
        assert_eq!(TempoParam::Accent(last).index(), 22);
        let mut tempo = controller(1.0);
        // The last beat is a plain unset slot, not an out-of-bounds index.
        assert_eq!(tempo.driver_value(TempoParam::Accent(last)), None);
        assert!(
            tempo
                .global_tempo_update(TempoParam::Accent(last))
                .is_none()
        );
        tempo.apply(22, 1.0, Source::Local).unwrap();
        assert_eq!(tempo.driver_value(TempoParam::Accent(last)), Some(1.0));
        assert_eq!(
            tempo
                .global_tempo_update(TempoParam::Accent(last))
                .unwrap()
                .first_indexed_float(),
            Some((22, 1.0))
        );
    }

    #[test]
    fn spans_and_dsp_outputs_are_refused_before_any_state_changes() {
        let build = |range| {
            let mut ranges = unit_ranges();
            ranges[8] = Some(range);
            TempoController::new(
                TempoConfig {
                    ranges,
                    scene_mode: [false; PARAM_COUNT],
                },
                TempoState {
                    global: [None; PARAM_COUNT],
                    scenes: [[None; PARAM_COUNT]; SCENE_COUNT],
                    current_scene: 0,
                },
            )
            .err()
        };
        // Finite endpoints whose difference overflows, and a single-value range.
        for (min, max) in [(-f32::MAX, f32::MAX), (1.0, 1.0)] {
            assert_eq!(
                build(ParamRange {
                    min,
                    max,
                    skew: 1.0
                }),
                Some(TempoError::RangeSpan(8))
            );
        }
        // The public helper stays total: the stock 1.0 / 0.0 landing, never NaN.
        let single = ParamRange {
            min: 1.0,
            max: 1.0,
            skew: 1.0,
        };
        assert_eq!((single.normalize(1.0), single.normalize(0.5)), (1.0, 0.0));
        assert_eq!(single.denormalize(0.3), 1.0);

        // A finite 1000 dB driver value would flush as +inf: refused before the store.
        let mut tempo = controller(1.0);
        tempo.config.ranges[3] = Some(ParamRange {
            min: 0.0,
            max: 1000.0,
            skew: 1.0,
        });
        let before = tempo.state().clone();
        assert_eq!(
            tempo.apply(3, 1.0, Source::Remote),
            Err(TempoError::Unrepresentable {
                index: 3,
                real_bits: 1000f32.to_bits()
            })
        );
        assert_eq!(tempo.state(), &before);
        assert_eq!(tempo.driver_value(TempoParam::LevelDb), None);
        assert!(!tempo.flush_armed() && tempo.flush().unwrap().is_empty());
        let ok = tempo.apply(3, 0.5, Source::Remote).unwrap();
        assert_eq!(ok.real, Some(500.0));
        assert_eq!(
            frame_bytes(&tempo.flush().unwrap()),
            vec![parameter_bytes(1, 3, 10f32.powf(25.0))]
        );
    }

    #[test]
    fn flush_refusal_keeps_every_dirty_flag() {
        let mut tempo = controller(1.0);
        tempo.apply(4, 0.0, Source::Local).unwrap();
        tempo.apply(0, 1.0, Source::Local).unwrap();
        tempo.apply(9, 0.0, Source::Local).unwrap();
        // Only a slot written behind `apply` can hold an unrepresentable value.
        tempo.driver[3] = DriverSlot {
            value: Some(1000.0),
            dirty: true,
        };
        assert_eq!(
            tempo.flush(),
            Err(TempoError::Unrepresentable {
                index: 3,
                real_bits: 1000f32.to_bits()
            })
        );
        assert!(tempo.flush_armed());
        assert!([0, 3, 4, 9].iter().all(|&i| tempo.driver[i].dirty));
        // Repairing the slot flushes everything, still enable-off first.
        tempo.driver[3].value = Some(0.0);
        let frames = frame_bytes(&tempo.flush().unwrap());
        let mut destination = vec![0x46, 1, 0, 0, 9, 0, 0, 0, 0x14, 0, 0, 0, 0, 0, 0, 0];
        destination.extend_from_slice(&19f32.to_le_bytes());
        assert_eq!(
            frames,
            vec![
                parameter_bytes(4, 4, 0.0),
                parameter_bytes(4, 0, 240.0),
                parameter_bytes(1, 3, 1.0),
                destination,
            ]
        );
        assert!(!tempo.flush_armed() && tempo.driver.iter().all(|s| !s.dirty));
    }

    #[test]
    fn initial_stores_refuse_non_finite_values() {
        let build = |global: Store, scenes: [Store; SCENE_COUNT]| {
            TempoController::new(
                TempoConfig {
                    ranges: unit_ranges(),
                    scene_mode: [false; PARAM_COUNT],
                },
                TempoState {
                    global,
                    scenes,
                    current_scene: 0,
                },
            )
            .err()
        };
        let empty = [[None; PARAM_COUNT]; SCENE_COUNT];
        let mut global = [None; PARAM_COUNT];
        global[1] = Some(1.0);
        assert_eq!(build(global, empty), None);
        global[1] = Some(f32::INFINITY);
        assert_eq!(
            build(global, empty),
            Some(TempoError::GlobalNonFinite {
                index: 1,
                bits: f32::INFINITY.to_bits()
            })
        );
        global[1] = Some(1.0);
        global[0] = Some(f32::from_bits(0x7fc0_0001));
        assert_eq!(
            build(global, empty),
            Some(TempoError::GlobalNonFinite {
                index: 0,
                bits: 0x7fc0_0001
            })
        );
        global[0] = None;
        let mut scenes = empty;
        scenes[5][7] = Some(f32::NEG_INFINITY);
        assert_eq!(
            build(global, scenes),
            Some(TempoError::SceneNonFinite {
                scene: 5,
                index: 7,
                bits: f32::NEG_INFINITY.to_bits()
            })
        );
    }
    /// Every driver-reaching index set, with distinct values for the slots the resend
    /// transforms: 120 BPM, -17 dB, pan -0.5, 4/4, subdivision 1, destination choice 2,
    /// enable on, accent beat 0 = 1, every other accent 0.
    fn full_store() -> Store {
        let mut store = [Some(0.0); PARAM_COUNT];
        store[0] = Some(0.4);
        store[3] = Some(0.5);
        store[4] = Some(1.0);
        store[5] = Some(0.25);
        store[6] = Some(0.1);
        store[7] = Some(1.0);
        store[9] = Some(0.5);
        store[10] = Some(1.0);
        store
    }

    fn block_bytes(values: [f32; PARAM_COUNT]) -> Vec<u8> {
        let mut bytes = vec![0x46, 4, 0, 0, 0, 0, 0, 0, 108, 0, 0, 0, 92, 0, 0, 0];
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    fn destination_bytes(value: f32) -> Vec<u8> {
        let mut bytes = vec![0x46, 1, 0, 0, 9, 0, 0, 0, 0x14, 0, 0, 0, 0, 0, 0, 0];
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes
    }

    /// Wire view of a plan: frame bytes with their policy, or `None` for a barrier.
    fn wire(plan: &ResendPlan) -> Vec<Option<(Vec<u8>, SendPolicy)>> {
        plan.steps
            .iter()
            .map(|step| match step {
                ResendStep::Request { frame, policy } => Some((frame.as_bytes().to_vec(), *policy)),
                ResendStep::SyncAllCores => None,
                ResendStep::Block(block) => Some((block.to_frame(), SendPolicy::POST)),
            })
            .collect()
    }

    #[test]
    fn resend_after_mode_change_follows_the_stock_order_and_raw_block() {
        let mut tempo = controller(0.0);
        // The scene store holds other values; a plan that reads it would show 240 BPM.
        tempo.state.scenes[2] = [Some(1.0); PARAM_COUNT];
        tempo.state.global = full_store();
        tempo.state.global[1] = Some(0.0);
        let switched = tempo.apply(1, 1.0, Source::Local).unwrap();
        assert_eq!(switched.effect, Some(Effect::ResendAll));

        let plan = tempo.plan_resend().unwrap();
        let level = 10f32.powf(-17.0 / 20.0);
        let mut raw = [0.0f32; PARAM_COUNT];
        raw[0] = 120.0;
        raw[3] = -17.0;
        raw[4] = 1.0;
        raw[5] = -0.5;
        raw[6] = 2.0;
        raw[7] = 1.0;
        raw[9] = 2.0;
        raw[10] = 1.0;
        let mut zeroed_enable = raw;
        zeroed_enable[4] = 0.0;
        assert_eq!(
            wire(&plan),
            vec![
                Some((parameter_bytes(4, 4, 0.0), SendPolicy::WAIT)),
                None,
                Some((destination_bytes(1.0), SendPolicy::POST)),
                Some((parameter_bytes(1, 5, 0.25), SendPolicy::POST)),
                Some((parameter_bytes(1, 3, level), SendPolicy::POST)),
                Some((block_bytes(zeroed_enable), SendPolicy::POST)),
                None,
                Some((parameter_bytes(4, 4, 1.0), SendPolicy::WAIT)),
            ]
        );
        assert!(plan.rerouted);
        assert_eq!(plan.time_signature, TimeSignature::from_index(2));
        assert_eq!(
            (
                SendPolicy::WAIT.wait,
                SendPolicy::SYNC.wait,
                SendPolicy::POST.wait
            ),
            (true, true, false)
        );
        assert_eq!(SendPolicy::SYNC.timeout, Duration::from_millis(1000));
        // Driver values hold the raw floats; every slot is clean and no flush waits.
        assert_eq!(tempo.driver_value(TempoParam::LevelDb), Some(-17.0));
        assert_eq!(tempo.driver_value(TempoParam::Pan), Some(-0.5));
        assert!(tempo.driver.iter().all(|slot| !slot.dirty));
        assert!(!tempo.flush_armed() && tempo.flush().unwrap().is_empty());
        assert_eq!(tempo.state().scenes[2][0], Some(1.0));

        // Unchanged destination and pan: only the level frame and the block, enable real.
        let again = tempo.plan_resend().unwrap();
        assert!(!again.rerouted);
        assert_eq!(
            wire(&again),
            vec![
                Some((parameter_bytes(1, 3, level), SendPolicy::POST)),
                Some((block_bytes(raw), SendPolicy::POST)),
            ]
        );

        // Back to preset mode: the current scene drives every slot.
        tempo.apply(1, 0.0, Source::Local).unwrap();
        let preset = tempo.plan_resend().unwrap();
        assert!(preset.rerouted);
        assert_eq!(tempo.driver_value(TempoParam::Bpm), Some(240.0));
        let Some(ResendStep::Block(block)) = preset
            .steps
            .iter()
            .find(|step| matches!(step, ResendStep::Block(_)))
        else {
            panic!("plan carries a block");
        };
        assert_eq!(block.values_bits()[0], 240f32.to_bits());
        assert_eq!(block.values_bits()[4], 0f32.to_bits());
    }

    #[test]
    fn resend_refuses_missing_state_without_touching_the_driver() {
        let mut tempo = controller(1.0);
        tempo.apply(0, 0.4, Source::Local).unwrap();
        // Index 3 is the first driver-reaching slot after 0 without a global value.
        assert_eq!(tempo.plan_resend(), Err(TempoError::GlobalUnset(3)));
        assert!(tempo.flush_armed() && tempo.driver[0].dirty);
        assert_eq!(tempo.driver_value(TempoParam::Bpm), Some(120.0));
        assert_eq!(tempo.driver_value(TempoParam::LevelDb), None);

        let mut preset = controller(0.0);
        preset.state.scenes[2] = full_store();
        preset.state.scenes[2][22] = None;
        assert_eq!(
            preset.plan_resend(),
            Err(TempoError::SceneUnset {
                scene: 2,
                index: 22
            })
        );
        preset.state.scenes[2][22] = Some(1.0);
        preset.config.ranges[8] = None;
        assert_eq!(
            preset.plan_resend(),
            Err(TempoError::RangeUnknown(TempoParam::Sound))
        );
        preset.config.ranges = unit_ranges();
        preset.state.global[1] = None;
        assert_eq!(preset.plan_resend(), Err(TempoError::ModeUnset));
        assert!(
            preset
                .driver
                .iter()
                .skip(3)
                .all(|slot| slot.value.is_none())
        );
        assert!(!preset.flush_armed());

        // A finite 1000 dB store value has no finite DSP float: refused with the same inertia.
        preset.state.global[1] = Some(0.0);
        preset.config.ranges[3] = Some(ParamRange {
            min: 0.0,
            max: 1000.0,
            skew: 1.0,
        });
        preset.state.scenes[2][3] = Some(1.0);
        assert_eq!(
            preset.plan_resend(),
            Err(TempoError::Unrepresentable {
                index: 3,
                real_bits: 1000f32.to_bits()
            })
        );
        assert!(
            preset
                .driver
                .iter()
                .skip(3)
                .all(|slot| slot.value.is_none())
        );
        preset.config.ranges = unit_ranges();
        assert_eq!(preset.plan_resend().unwrap().steps.len(), 8);
    }

    #[test]
    fn driver_initialization_is_complete_finite_and_transactional() {
        let mut tempo = controller(1.0);
        let mut values = [0.0f32; PARAM_COUNT];
        values[0] = 120.0;
        values[3] = -17.0;
        values[5] = -0.5;
        values[6] = 2.0;
        values[7] = 1.0;
        values[9] = 2.0;
        values[10] = 1.0;
        values[22] = f32::NAN;
        assert_eq!(
            tempo.initialize_driver(values),
            Err(TempoError::DriverNonFinite {
                index: 22,
                bits: f32::NAN.to_bits()
            })
        );
        assert_eq!(tempo.driver_value(TempoParam::Bpm), None);
        values[22] = 0.0;

        tempo.apply(0, 1.0, Source::Local).unwrap();
        assert_eq!(
            tempo.initialize_driver(values),
            Err(TempoError::DriverDirty)
        );
        assert_eq!(tempo.driver_value(TempoParam::Bpm), Some(240.0));
        assert_eq!(tempo.flush().unwrap().len(), 1);

        tempo.initialize_driver(values).unwrap();
        assert_eq!(tempo.driver_value(TempoParam::Bpm), Some(120.0));
        assert_eq!(tempo.driver_value(TempoParam::Destination), Some(2.0));
        assert!(!tempo.flush_armed() && tempo.flush().unwrap().is_empty());

        // A store that matches the declared driver leaves destination and pan clean, so
        // the resend skips the off / sync / restore steps and keeps enable in the block.
        tempo.state.global = full_store();
        tempo.state.global[1] = Some(1.0);
        tempo.state.global[4] = Some(0.0);
        let plan = tempo.plan_resend().unwrap();
        assert!(!plan.rerouted);
        assert_eq!(plan.steps.len(), 2);
        let ResendStep::Block(block) = &plan.steps[1] else {
            panic!("second step is the block");
        };
        assert_eq!(block.values_bits()[4], 0f32.to_bits());
        assert_eq!(block.values_bits()[9], 2f32.to_bits());
    }

    /// MIDI clock BPM (`0x556e3c` alone): the slot flushes after an enable-off frame like any
    /// dirty slot, the store and echo stay untouched, and an unchanged value arms nothing.
    #[test]
    fn driver_setter_flushes_without_a_store_write() {
        let mut tempo = controller(1.0);
        tempo.apply(4, 0.0, Source::Local).unwrap();
        let store = tempo.state().clone();
        assert_eq!(tempo.set_driver(TempoParam::Bpm, 133.0), Ok(true));
        assert_eq!(tempo.state().global, store.global);
        assert!(tempo.flush_armed());
        assert_eq!(
            frame_bytes(&tempo.flush().unwrap()),
            [parameter_bytes(4, 4, 0.0), parameter_bytes(4, 0, 133.0)]
        );
        assert_eq!(tempo.set_driver(TempoParam::Bpm, 133.0), Ok(false));
        assert_eq!(tempo.set_driver(TempoParam::TempoMode, 0.0), Ok(false));
        assert!(!tempo.flush_armed());
    }
}
