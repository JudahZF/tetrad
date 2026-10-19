//! Settings catalog: typed setting identities, their stock labels and value
//! shapes, and the page structure of the settings and I/O screens.
//!
//! Labels and most option lists are ZenUI strings (VAs in the g-ui doc).
//! A `Choice` index maps to the stored stock value as documented on each
//! list; owned lists say so. Ranges marked `bounds: None` are not recovered;
//! the application clamps them and refuses unknown values.

use crate::types::Row;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InputJack {
    In1,
    In2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OutputJack {
    Out12,
    Out34,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LoopJack {
    One,
    Two,
}

/// Global EQ band 0..5 (Parametric-5, manual §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EqBand(pub(crate) u8);

impl EqBand {
    pub const COUNT: u8 = 5;

    pub fn new(band: u8) -> Option<Self> {
        (band < Self::COUNT).then_some(Self(band))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    pub(crate) fn all() -> impl Iterator<Item = Self> {
        (0..Self::COUNT).map(Self)
    }
}

/// Global EQ page tab: a band or OUT (assignment and output level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EqTab {
    Band(EqBand),
    Out,
}

/// Every setting the owned UI can display or request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingId {
    /// "Don't show again" of the Excessive Downloads dialog (`0x22bd50`;
    /// `dontShowExcess{Capture,Preset}Downloads`).
    DontShowExcessDownloads(ExcessDownloads),
    ScreenBrightness,
    LedBrightness,
    DimmedLedBrightness,
    PresetDimmed,
    SceneDimmed,
    StompDimmed,
    LockScreen,
    PowerButtonSensitivity,
    PowerOnMode,
    MasterVolumeOut12,
    MasterVolumeOut34,
    MasterVolumeSend12,
    MasterVolumeHeadphones,
    InternetCheck,
    /// Connection › Domain Settings (`0x40c4bc`): stored `cloudDomain`.
    CloudDomain,
    GlobalBypassCab(Row),
    GlobalBypassIr(Row),
    SceneBypassBehavior,
    StompModeAutoAssign,
    HoldTiming,
    SwapTempoTuner,
    GigViewAccess,
    LatencyCompensation,
    MidiChannel,
    MidiOverUsb,
    IgnoreDuplicatePc,
    MidiClockOut,
    MidiClockIn,
    MidiThru,
    ReloadPreset,
    ReloadScene,
    InputLevel(InputJack),
    InputImpedance(InputJack),
    InputType(InputJack),
    Phantom(InputJack),
    InputGroundLift(InputJack),
    OutputLevel(OutputJack),
    /// OUT 1/2 only: stock OUT 3/4 has no ground lift (w4-iosettings §1.1).
    OutputGroundLift(OutputJack),
    OutputMute(OutputJack),
    /// Stereo link of a jack pair (`output_0_paired` / `output_1_paired`). Off shows
    /// the two mono jacks; the owned page keeps one row set per pair (I).
    OutputPair(OutputJack),
    SendLevel(LoopJack),
    ReturnLevel(LoopJack),
    ReturnGroundLift(LoopJack),
    HeadphoneLevel,
    /// Headphones feed level (1700..1709), −40..12 dB as the output levels.
    HeadphoneFeed(crate::HpFeed),
    HeadphoneSource,
    UsbLevel,
    UsbRouting,
    TunerReference,
    TunerMute,
    LiveTuner,
    TunerInput,
    TempoMode,
    BpmNote,
    Metronome,
    TimeSignature,
    Subdivision,
    TempoLed,
    MetronomeSound,
    MetronomeRouting,
    MetronomeLevel,
    /// Metronome PAN (model 25000 parameter 5, −1 L … +1 R, 360 steps).
    MetronomePan,
    /// Global EQ (model 4004 "Output Equalizer", w4-iosettings §5): ON button, five bands,
    /// OUTPUT −20..20 dB and the output pair assignment (ASSIGN_EQ_0/1).
    EqEnabled,
    EqType(EqBand),
    EqGain(EqBand),
    EqFreq(EqBand),
    EqQ(EqBand),
    EqBypass(EqBand),
    EqOutput,
    EqAssign(OutputJack),
}

/// Current value supplied by the application, or a requested new value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingValue {
    Bool(bool),
    /// Index into the setting's option list.
    Choice(u16),
    /// Fixed-point: real value × 10^decimals of the setting's `Number` kind.
    Number(i32),
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Toggle,
    Choice(&'static [&'static str]),
    Number {
        step: i32,
        decimals: u8,
        unit: &'static str,
        bounds: Option<(i32, i32)>,
    },
}

const PERCENT: SettingKind = SettingKind::Number {
    step: 5,
    decimals: 0,
    unit: "%",
    bounds: Some((0, 100)),
};
const DB: SettingKind = SettingKind::Number {
    step: 5,
    decimals: 1,
    unit: " dB",
    bounds: None,
};
/// Level bounds in tenths of dB (w4-iosettings §1.1). The mini IN/RET maximum is 30 dB;
/// the application clamps and snaps to the stock steps.
const fn level(min: i32, max: i32) -> SettingKind {
    SettingKind::Number {
        step: 5,
        decimals: 1,
        unit: " dB",
        bounds: Some((min, max)),
    }
}
const ON_OFF: SettingKind = SettingKind::Toggle;

impl SettingId {
    pub fn label(self) -> &'static str {
        match self {
            Self::DontShowExcessDownloads(_) => "Don't show again",
            Self::ScreenBrightness => "Screen",
            Self::LedBrightness => "LEDs",
            Self::DimmedLedBrightness => "Dimmed LEDs",
            Self::PresetDimmed => "Preset",
            Self::SceneDimmed => "Scene",
            Self::StompDimmed => "Stomp",
            Self::LockScreen => "Lock Touchscreen and Master Volume",
            Self::PowerButtonSensitivity => "Power Button Sensitivity",
            Self::PowerOnMode => "Power On Mode",
            Self::MasterVolumeOut12 => "OUT 1/2",
            Self::MasterVolumeOut34 => "OUT 3/4",
            Self::MasterVolumeSend12 => "SEND 1/2",
            Self::MasterVolumeHeadphones => "Headphones",
            Self::InternetCheck => "Internet Check",
            Self::CloudDomain => "Domain Settings",
            Self::GlobalBypassCab(row) | Self::GlobalBypassIr(row) => {
                ["ROW 1", "ROW 2", "ROW 3", "ROW 4"][usize::from(row.get())]
            }
            Self::SceneBypassBehavior => "Scene Bypass State Behavior",
            Self::StompModeAutoAssign => "AUTO-ASSIGN",
            Self::HoldTiming => "Hold Timing",
            Self::SwapTempoTuner => "SWAP TEMPO AND TUNER",
            Self::GigViewAccess => "Gig View Footswitch Access",
            Self::LatencyCompensation => "Dynamic Latency Compensation",
            Self::MidiChannel => "MIDI Channel",
            Self::MidiOverUsb => "MIDI Over USB",
            Self::IgnoreDuplicatePc => "Ignore Duplicate PC",
            Self::MidiClockOut => "MIDI Clock Out",
            Self::MidiClockIn => "MIDI Clock In",
            Self::MidiThru => "MIDI Thru",
            Self::ReloadPreset => "Reload an active Preset",
            Self::ReloadScene => "Reload an active Scene",
            Self::InputLevel(InputJack::In1) => "IN 1 LEVEL",
            Self::InputLevel(InputJack::In2) => "IN 2 LEVEL",
            Self::InputImpedance(_) => "IMPEDANCE",
            Self::InputType(_) => "TYPE",
            Self::Phantom(_) => "PHANTOM 48V",
            Self::InputGroundLift(_) | Self::OutputGroundLift(_) | Self::ReturnGroundLift(_) => {
                "GROUND LIFT"
            }
            Self::OutputLevel(OutputJack::Out12) => "OUT 1/2 LEVEL",
            Self::OutputLevel(OutputJack::Out34) => "OUT 3/4 LEVEL",
            Self::OutputMute(_) => "MUTE",
            Self::OutputPair(OutputJack::Out12) => "OUT 1/2 LINK",
            Self::OutputPair(OutputJack::Out34) => "OUT 3/4 LINK",
            Self::SendLevel(LoopJack::One) => "SEND 1 LEVEL",
            Self::SendLevel(LoopJack::Two) => "SEND 2 LEVEL",
            Self::ReturnLevel(LoopJack::One) => "RET 1 LEVEL",
            Self::ReturnLevel(LoopJack::Two) => "RET 2 LEVEL",
            Self::HeadphoneLevel => "HP LEVEL",
            Self::HeadphoneFeed(feed) => feed.label(),
            Self::HeadphoneSource => "HP SOURCE",
            Self::UsbLevel => "USB LEVEL",
            Self::UsbRouting => "DRY - WET",
            Self::TunerReference => "FREQ [Hz]",
            Self::TunerMute => "MUTE",
            Self::LiveTuner => "LIVE TUNER",
            Self::TunerInput => "INPUT",
            Self::TempoMode => "Tempo Mode",
            Self::BpmNote => "BPM =",
            Self::Metronome => "METRONOME",
            Self::TimeSignature => "T/SIGNATURE",
            Self::Subdivision => "SUBDIVISIONS",
            Self::TempoLed => "TEMPO LED",
            Self::MetronomeSound => "SOUND",
            Self::MetronomeRouting => "ROUTING",
            Self::MetronomeLevel => "LEVEL",
            Self::MetronomePan => "PAN",
            Self::EqEnabled => "GLOBAL EQ",
            Self::EqType(_) => "TYPE",
            Self::EqGain(_) => "GAIN",
            Self::EqFreq(_) => "FREQ",
            Self::EqQ(_) => "Q",
            Self::EqBypass(band) => {
                ["BYPASS 1", "BYPASS 2", "BYPASS 3", "BYPASS 4", "BYPASS 5"][usize::from(band.0)]
            }
            Self::EqOutput => "OUTPUT",
            Self::EqAssign(OutputJack::Out12) => "OUT 1/2",
            Self::EqAssign(OutputJack::Out34) => "OUT 3/4",
        }
    }

    /// Knob pointer position (0..=1 of the 270° sweep) at `raw`. Stock knobs show the
    /// catalog proportion `((v − min) / (max − min))^skew` (tetrad-core `curve` mode 1): Tempo
    /// VOLUME is ModelRepo 25000 −60…+9 dB skew 3.5 (0.0 dB at 300°, `mi4`), a Global EQ FREQ
    /// is 4004 20…20 000 Hz skew 0.3 (stock stores normalized catalog values, `0x2fa580`). The
    /// other Settings knobs are linear over their bounds: I/O levels (31000 floats, `0x64b8f8`
    /// skew 1), Global EQ GAIN, Q and OUTPUT, Tempo PAN, the tuner reference.
    pub fn knob_position(self, raw: i32) -> f32 {
        let Some((min, max, skew)) = self.knob_range() else {
            return 0.0;
        };
        let u = ((raw - min) as f32 / (max - min).max(1) as f32).clamp(0.0, 1.0);
        if skew == 1.0 { u } else { u.powf(skew) }
    }

    /// The raw value at knob `position`, the inverse of [`Self::knob_position`], rounded to
    /// the setting's step: a knob drag moves the position (stock stores the normalized value;
    /// the rounding is Tetrad's fixed-point store, I). `None` for settings without a knob range.
    pub(crate) fn raw_at(self, position: f32) -> Option<i32> {
        let (SettingKind::Number { step, .. }, Some((min, max, skew))) =
            (self.kind(), self.knob_range())
        else {
            return None;
        };
        let position = position.clamp(0.0, 1.0);
        let u = if skew == 1.0 {
            position
        } else {
            position.powf(skew.recip())
        };
        let raw = min as f32 + u * (max - min) as f32;
        let step = step.max(1) as f32;
        Some((((raw / step).round() * step) as i32).clamp(min, max))
    }

    /// Catalog range and skew of a knob setting (see [`Self::knob_position`]).
    fn knob_range(self) -> Option<(i32, i32, f32)> {
        match (self, self.kind()) {
            (Self::MetronomeLevel, _) => Some((-600, 90, 3.5)),
            (Self::EqFreq(_), _) => Some((20, 20_000, 0.3)),
            (
                _,
                SettingKind::Number {
                    bounds: Some((min, max)),
                    ..
                },
            ) => Some((min, max, 1.0)),
            _ => None,
        }
    }

    pub fn kind(self) -> SettingKind {
        match self {
            Self::DontShowExcessDownloads(_) => SettingKind::Toggle,
            Self::ScreenBrightness | Self::LedBrightness | Self::DimmedLedBrightness => PERCENT,
            // Index = stored `powerButtonSensitivity` 0..=3 (tetrad-settings).
            Self::PowerButtonSensitivity => SettingKind::Choice(&["Off", "Low", "Medium", "High"]),
            // Index 0 neuraldsp.com, 1 wpic-tools.com (`0x40cd74`).
            Self::CloudDomain => SettingKind::Choice(&["Default Domain", "Alternative Domain"]),
            Self::PowerOnMode => SettingKind::Choice(&["Manual", "Automatic"]),
            Self::SceneBypassBehavior => SettingKind::Choice(&[
                "Always overwrite",
                "Overwrite non-stomp blocks",
                "Never overwrite",
            ]),
            Self::HoldTiming => {
                SettingKind::Choice(&["500ms", "600ms", "700ms", "800ms", "900ms", "1000ms"])
            }
            // `0x00416674`: 1..16, then OMNI. Index = stored `Channel` − 1
            // (17 = OMNI).
            Self::MidiChannel => SettingKind::Choice(&[
                "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15",
                "16", "OMNI",
            ]),
            // Index = stored `ClockOutEn`. `0x00416674` adds "OFF",
            // `0x00415d00` ("DIN" on the QC), "USB" and "USB+%1".
            Self::MidiClockOut => SettingKind::Choice(&["OFF", "DIN", "USB", "USB+DIN"]),
            Self::InputLevel(_) | Self::ReturnLevel(_) => level(-120, 600),
            Self::OutputLevel(_)
            | Self::SendLevel(_)
            | Self::HeadphoneLevel
            | Self::HeadphoneFeed(_) => level(-400, 120),
            Self::UsbLevel => level(-400, 0),
            Self::MetronomeLevel => DB,
            // Hundredths of the −1..1 range; text "50 L", "C", "26 R" (`0x5cd41c`).
            Self::MetronomePan => SettingKind::Number {
                step: 1,
                decimals: 2,
                unit: "",
                bounds: Some((-100, 100)),
            },
            Self::InputImpedance(_) => SettingKind::Choice(&[
                "10k", "33k", "68k", "91k", "130k", "232k", "470k", "1M", "10M",
            ]),
            Self::InputType(_) => SettingKind::Choice(&["Instrument", "Mic"]),
            // Index = stored option index (ctor `0x574f8`, DSP `0x33`).
            Self::HeadphoneSource => SettingKind::Choice(&["NONE", "USB 1/2", "USB 3/4", "BOTH"]),
            // "DRY - WET" (`0x57940`, DSP `0x39`): which USB pair carries the dry signal.
            Self::UsbRouting => SettingKind::Choice(&["1/2 - 3/4", "3/4 - 1/2"]),
            // Recovered MIN/MAX 400/480 Hz.
            Self::TunerReference => SettingKind::Number {
                step: 1,
                decimals: 0,
                unit: " Hz",
                bounds: Some((400, 480)),
            },
            // `InputPortId::TUNER_CHOICES` order; labels from the stock port table (`0x48b5c`).
            Self::TunerInput => SettingKind::Choice(&[
                "Input 1",
                "Input 2",
                "Input 1/2",
                "Return 1",
                "Return 2",
                "USB input 5",
                "USB input 6",
            ]),
            Self::TempoMode => SettingKind::Choice(&["Global", "Preset"]),
            Self::BpmNote => SettingKind::Choice(&["QUARTERS", "EIGHTHS"]),
            Self::TimeSignature => SettingKind::Choice(&[
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
            ]),
            Self::Subdivision => {
                SettingKind::Choice(&["1/4", "1/8", "1/8T", "1/16", "1/16T", "1/32"])
            }
            // Owned lists: no stock strings exist for metronome sounds or
            // routing. The application maps them.
            Self::MetronomeSound => SettingKind::Choice(&[
                "BLIP", "BLOCK", "COWBELL", "DIGITAL", "DRUM KIT", "SOFT KIT",
            ]),
            Self::MetronomeRouting => {
                SettingKind::Choice(&["MULTI", "HP", "OUT 1/2", "OUT 3/4", "SEND 1/2"])
            }
            // Labels `0x11e87a4..0x11e87c0`. Stock steps are PEAK, HI PASS, LO PASS,
            // HI SHELF, LO SHELF (`0x639f34`); tetrad-app maps this list onto them.
            Self::EqType(_) => {
                SettingKind::Choice(&["HI PASS", "LO PASS", "HI SHELF", "LO SHELF", "PEAK"])
            }
            // Manual §3: GAIN −12/+12 dB, FREQ 20 Hz/20 kHz; Q 0.1..10 (model 4004).
            Self::EqGain(_) => level(-120, 120),
            Self::EqFreq(_) => SettingKind::Number {
                step: 1,
                decimals: 0,
                unit: " Hz",
                bounds: Some((20, 20_000)),
            },
            Self::EqQ(_) => SettingKind::Number {
                step: 5,
                decimals: 2,
                unit: "",
                bounds: Some((10, 1000)),
            },
            Self::EqOutput => level(-200, 200),
            _ => ON_OFF,
        }
    }

    /// Stock (i) help text (MIDI page `0x7cb78`, w4 spec §4). `%1` is filled with the QC
    /// values ("Quad Cortex", "DIN"); rich text is flattened to plain lines.
    pub fn help(self) -> Option<&'static str> {
        Some(match self {
            Self::MidiChannel => "Sets the Quad Cortex base MIDI channel.",
            Self::MidiThru => {
                "MIDI Thru allows many devices to have their MIDI connections daisy chained \
                 together all being driven by a common MIDI source."
            }
            Self::MidiOverUsb => "Enable or disable MIDI over USB.",
            Self::IgnoreDuplicatePc => {
                "Enabled:\nA Preset will not be reloaded if the corresponding MIDI Program \
                 Change message is sent. CC#0 and CC#32 messages will also be ignored.\n\n\
                 Disabled:\nA Preset will be reloaded if the corresponding MIDI Program Change \
                 message is sent."
            }
            Self::MidiClockOut => {
                "Enable or disable MIDI Clock Out for USB or DIN.\n\nNote that \"MIDI Over \
                 USB\" must be set to \"ON\" to enable MIDI Clock Out on USB."
            }
            Self::MidiClockIn => {
                "Enable or disable MIDI Clock In for USB or DIN.\n\nNote that \"MIDI Over \
                 USB\" must be set to \"ON\" to enable MIDI Clock In via USB."
            }
            _ => return None,
        })
    }

    /// Display text for a value, or a dash when the application has none.
    pub fn format(self, value: Option<&SettingValue>) -> String {
        match (self.kind(), value) {
            (SettingKind::Toggle, Some(SettingValue::Bool(on))) => {
                if *on { "On" } else { "Off" }.into()
            }
            (SettingKind::Choice(options), Some(SettingValue::Choice(index))) => options
                .get(usize::from(*index))
                .map_or_else(|| "-".into(), |option| (*option).into()),
            (SettingKind::Number { .. }, Some(SettingValue::Number(raw)))
                if matches!(self, Self::MetronomePan) =>
            {
                // `0x5cd41c` with the model 25000 PAN labels: p = 50·v, "C" within 0.5,
                // else the rounded magnitude, a space and the side ("50 L").
                let p = *raw as f32 / 2.0;
                match p {
                    p if p.abs() < 0.5 => "C".into(),
                    p if p < 0.0 => format!("{} L", -p.round()),
                    p => format!("{} R", p.round()),
                }
            }
            (SettingKind::Number { decimals, unit, .. }, Some(SettingValue::Number(raw))) => {
                format!("{}{unit}", fixed(*raw, decimals))
            }
            (_, Some(SettingValue::Text(text))) => text.clone(),
            _ => "-".into(),
        }
    }

    /// The value one step from `current` (wrapping choices, clamped numbers,
    /// flipped toggles). `None` when the current value is unknown.
    pub(crate) fn step(self, current: Option<&SettingValue>, steps: i32) -> Option<SettingValue> {
        match (self.kind(), current?) {
            (SettingKind::Toggle, SettingValue::Bool(on)) => Some(SettingValue::Bool(!on)),
            (SettingKind::Choice(options), SettingValue::Choice(index)) => {
                let count = options.len() as i32;
                Some(SettingValue::Choice(
                    (i32::from(*index) + steps).rem_euclid(count) as u16,
                ))
            }
            (SettingKind::Number { step, bounds, .. }, SettingValue::Number(raw)) => {
                let next = raw.saturating_add(step.saturating_mul(steps));
                Some(SettingValue::Number(match bounds {
                    Some((min, max)) => next.clamp(min, max),
                    None => next,
                }))
            }
            _ => None,
        }
    }
}

/// Fixed-point integer to text, e.g. (−35, 1) → "-3.5".
pub(crate) fn fixed(raw: i32, decimals: u8) -> String {
    if decimals == 0 {
        return raw.to_string();
    }
    let scale = 10i64.pow(u32::from(decimals));
    let value = i64::from(raw);
    let sign = if value < 0 { "-" } else { "" };
    format!(
        "{sign}{}.{:0width$}",
        value.abs() / scale,
        value.abs() % scale,
        width = usize::from(decimals)
    )
}

/// Device-level actions that are requests, not settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceAction {
    ResetSettings,
    FactoryReset,
    ResetWifi,
    SendReport,
    Reboot,
    Shutdown,
    CalibrateExpression(u8),
    CloudLogout,
    /// Recovery CANCEL: leave the recovery screen and boot normally (manual §12, I).
    ContinueBoot,
    /// Preset MIDI Out CLEAR ALL. Confirmed in the UI, which then sends one empty
    /// `SetPresetMidi` per assigned target; the application never receives it.
    ClearPresetMidi,
    /// `LicenseRefreshButton` (`0x976058`): fetch the plugin licences of the linked account.
    RefreshLicences,
}

/// Settings pages in stock menu order (`0x791ac`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingsPage {
    MyAccount,
    Backups,
    Connection,
    Updates,
    Brightness,
    PowerFunctions,
    PowerOnMode,
    MasterVolume,
    DeviceStorage,
    FactoryReset,
    /// Owned (Tetrad): installed extensions, the catalog and its repositories.
    Extensions,
    GlobalBypass,
    SceneBypass,
    StompModeBypass,
    HoldTiming,
    SwapTempoTuner,
    GigViewAccess,
    LatencyCompensation,
    Midi,
    AboutContact,
    DeviceInformation,
    SendReport,
    Diagnostics,
    Licenses,
}

/// Groups of the settings dropdown (manual §10: Account, System, Device, Support).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingsGroup {
    Account,
    System,
    Device,
    Support,
}

impl SettingsGroup {
    pub const ALL: [Self; 4] = [Self::Account, Self::System, Self::Device, Self::Support];

    pub fn title(self) -> &'static str {
        match self {
            Self::Account => "Account",
            Self::System => "System",
            Self::Device => "Device",
            Self::Support => "Support",
        }
    }

    /// Pages in stock menu order (`0x791ac`); Extensions is owned and ends System.
    pub fn pages(self) -> &'static [SettingsPage] {
        use SettingsPage::*;
        match self {
            Self::Account => &[MyAccount, Backups],
            Self::System => &[
                Connection,
                Updates,
                Brightness,
                PowerFunctions,
                PowerOnMode,
                MasterVolume,
                DeviceStorage,
                FactoryReset,
                Extensions,
            ],
            Self::Device => &[
                GlobalBypass,
                SceneBypass,
                StompModeBypass,
                HoldTiming,
                SwapTempoTuner,
                GigViewAccess,
                LatencyCompensation,
                Midi,
            ],
            Self::Support => &[
                AboutContact,
                DeviceInformation,
                SendReport,
                Diagnostics,
                Licenses,
            ],
        }
    }
}

/// Content row of a generic settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageRow {
    Header(&'static str),
    /// Help paragraph. `▲`, `▼` and `⏻` draw as inline footswitch and bypass icons, as the
    /// stock rich texts do (the font has no such glyphs).
    Text(&'static str),
    Setting(SettingId),
    Action(DeviceAction, &'static str),
    /// One option of a radio list: the setting, its option index and the stock text.
    Radio(SettingId, u16, &'static str),
}

impl SettingsPage {
    pub fn group(self) -> SettingsGroup {
        SettingsGroup::ALL
            .into_iter()
            .find(|group| group.pages().contains(&self))
            .unwrap_or(SettingsGroup::Account)
    }

    /// Content title (stock page titles, w4 spec §9 item 14); the list shows [`Self::title`].
    pub fn heading(self) -> &'static str {
        match self {
            Self::SceneBypass => "Scene Bypass State Behavior",
            Self::StompModeBypass => "Stomp Mode Bypass Assignment",
            Self::SwapTempoTuner => "Swap Tempo and Tuner Access",
            Self::GigViewAccess => "Gig View Footswitch Access",
            Self::LatencyCompensation => "Dynamic Latency Compensation",
            Self::MasterVolume => "Master Volume Knob Assignment",
            Self::Midi => "MIDI Settings",
            Self::AboutContact => "About Us",
            Self::Licenses => "3rd party Licenses",
            page => page.title(),
        }
    }

    /// Page list label.
    pub fn title(self) -> &'static str {
        match self {
            Self::MyAccount => "My Account",
            Self::Backups => "Backups",
            Self::Connection => "Connection",
            Self::Updates => "Updates",
            Self::Brightness => "Brightness",
            Self::PowerFunctions => "Power Functions",
            Self::PowerOnMode => "Power On Mode",
            Self::MasterVolume => "Master Volume Knob",
            Self::DeviceStorage => "Device Storage",
            Self::FactoryReset => "Factory Reset",
            Self::Extensions => "Extensions",
            Self::GlobalBypass => "Global Bypass",
            Self::SceneBypass => "Scene Bypass Behavior",
            Self::StompModeBypass => "Stomp Mode Bypass",
            Self::HoldTiming => "Hold Timing",
            Self::SwapTempoTuner => "Swap Tempo and Tuner",
            Self::GigViewAccess => "Gig View Access",
            Self::LatencyCompensation => "Latency Compensation",
            Self::Midi => "MIDI",
            Self::AboutContact => "About and Contact",
            Self::DeviceInformation => "Device Information",
            Self::SendReport => "Send Report",
            Self::Diagnostics => "Diagnostics",
            Self::Licenses => "3rd Party Licenses",
        }
    }

    /// Rows for pages that are plain setting lists. Pages with their own layout (account,
    /// backups, Wi-Fi, updates, storage, information, brightness, global bypass, factory
    /// reset, extensions, about) return an empty list.
    pub(crate) fn rows(self) -> &'static [PageRow] {
        use PageRow::*;
        use SettingId as S;
        match self {
            Self::PowerFunctions => &[
                Setting(S::LockScreen),
                Setting(S::PowerButtonSensitivity),
                Header("Power"),
                Action(DeviceAction::Reboot, "REBOOT"),
                Action(DeviceAction::Shutdown, "SHUTDOWN"),
            ],
            // `0xa3cda4`, `0xa3cdf0`, `0xa3ce44` with %1 = Quad Cortex.
            Self::PowerOnMode => &[
                Text(
                    "Select the preferred startup behavior when connecting Quad Cortex to a power source:",
                ),
                Text(
                    "Manual Mode: Quad Cortex will remain off until the power button is manually pressed.",
                ),
                Text(
                    "Automatic Mode: Quad Cortex will power on immediately upon connection to a compatible power source (Default).",
                ),
                Setting(S::PowerOnMode),
            ],
            Self::MasterVolume => &[
                Text("Master Volume can control different outputs."),
                Setting(S::MasterVolumeOut12),
                Setting(S::MasterVolumeOut34),
                Setting(S::MasterVolumeSend12),
                Setting(S::MasterVolumeHeadphones),
            ],
            // `0xa34fa8`, `0xa34fd4`, `0xa35084`; index = `BypassSaveMode` order.
            Self::SceneBypass => &[
                Radio(
                    S::SceneBypassBehavior,
                    0,
                    "Always overwrite bypass state (default).",
                ),
                Radio(
                    S::SceneBypassBehavior,
                    1,
                    "Do not overwrite bypass state when changing\nbypass state via footswitches in Stomp Mode\n(including Hybrid Stomp Mode) or MIDI. Changes\nmade with the touchscreen will be saved.",
                ),
                Radio(
                    S::SceneBypassBehavior,
                    2,
                    "Do not overwrite bypass state when changing\nbypass state by any method.",
                ),
            ],
            // `0xa3da04`.
            Self::StompModeBypass => &[
                Text(
                    "A global setting for whether footswitches are automatically assigned to blocks' bypass settings. If you enable auto-assign, blocks will be assigned to footswitches in the order they are added to The Grid.",
                ),
                Setting(S::StompModeAutoAssign),
            ],
            // `0xa3bb38`.
            Self::HoldTiming => &[
                Text(
                    "Sets how long a footswitch must be held down to trigger its\nassigned HOLD action.",
                ),
                Setting(S::HoldTiming),
            ],
            // QC text `0xa3d5d0`; the bottom-right footswitch is TEMPO.
            Self::SwapTempoTuner => &[
                Text(
                    "Enable this option to access the Tuner by double-tapping the bottom-right footswitch, and access the Tempo settings by holding the bottom-right footswitch.",
                ),
                Setting(S::SwapTempoTuner),
            ],
            // `0xa3b6d8`; its inline footswitch icon is BANK DOWN (tetrad-core DOWN+TEMPO
            // chord, `0x20e4c0`).
            Self::GigViewAccess => &[
                Text(
                    "Enable this feature to toggle Gig View by pressing and holding ▼ and TEMPO footswitches simultaneously.\n\nWhen enabled, single-pressing ▼ + TEMPO to cycle MODES is triggered upon footswitches release.",
                ),
                Setting(S::GigViewAccess),
            ],
            // `0xa3d870`.
            Self::LatencyCompensation => &[
                Text(
                    "Disabling Dynamic Latency Compensation may help if phasing occurs when bypassing a device.",
                ),
                Setting(S::LatencyCompensation),
            ],
            // Stock order (`_INIT_324`, `0x7cb78`); reload block `0xa3cb18`..`0xa3cbbc`.
            Self::Midi => &[
                Setting(S::MidiChannel),
                Setting(S::MidiThru),
                Setting(S::MidiOverUsb),
                Setting(S::IgnoreDuplicatePc),
                Setting(S::MidiClockOut),
                Setting(S::MidiClockIn),
                Header("Preset and Scene Reload Behavior"),
                Text(
                    "Enabling this feature will allow an active Preset or Scene to\nbe reloaded via its assigned footswitch.",
                ),
                Setting(S::ReloadPreset),
                Setting(S::ReloadScene),
            ],
            Self::SendReport => &[
                Text("Send device diagnostics to support."),
                Action(DeviceAction::SendReport, "SEND REPORT"),
            ],
            Self::MyAccount
            | Self::Backups
            | Self::Connection
            | Self::Updates
            | Self::Brightness
            | Self::DeviceStorage
            | Self::FactoryReset
            | Self::Extensions
            | Self::GlobalBypass
            | Self::AboutContact
            | Self::DeviceInformation
            | Self::Diagnostics
            | Self::Licenses => &[],
        }
    }
}

/// I/O Settings connectors (rear-panel map, manual §3; stock `selected_connector`). A
/// port selects the parameter panel; USB has two pages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IoTab {
    #[default]
    Input1,
    Input2,
    Out12,
    Out34,
    Send,
    Return,
    Headphones,
    Usb,
    /// USB page "2" (owned content: DRY - WET; I).
    UsbRouting,
    Expression,
    Midi,
}

/// One cell of the I/O parameter panel (stock 5 × 2 card, shots 102, 049).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IoCell {
    Setting(SettingId),
    /// Level meter (stock ids 1710..1729, never stored), read from [`crate::IoMeters`].
    Meter(&'static str),
    /// USB channel pair meter with its (i) help (manual §9 channel map).
    UsbMeter(&'static str, &'static str),
    /// Headphones LEVEL meter (1730, a left/right pair, kind 6).
    HeadphoneMeter,
    /// A Headphones feed meter (1731..1740), mono or a pair by [`crate::HpFeed::jacks`].
    FeedMeter(crate::HpFeed),
    Text(&'static str),
    Action(DeviceAction, &'static str),
}

impl IoTab {
    pub const ALL: [Self; 11] = [
        Self::Input1,
        Self::Input2,
        Self::Out12,
        Self::Out34,
        Self::Send,
        Self::Return,
        Self::Headphones,
        Self::Usb,
        Self::UsbRouting,
        Self::Expression,
        Self::Midi,
    ];

    /// Green header name next to "I/O Settings" (shots 102 "Input 1", 049 "USB").
    pub fn title(self) -> &'static str {
        match self {
            Self::Input1 => "Input 1",
            Self::Input2 => "Input 2",
            Self::Out12 => "OUT 1/2",
            Self::Out34 => "OUT 3/4",
            Self::Send => "SEND 1/2",
            Self::Return => "RET 1/2",
            Self::Headphones => "Headphones",
            Self::Usb | Self::UsbRouting => "USB",
            Self::Expression => "EXP",
            Self::Midi => "MIDI",
        }
    }

    /// Panel rows (top, bottom) for the Grid row outputs `rows`.
    ///
    /// Headphones (`0x35a7f4`, connector 8): HP LEVEL in slot 0, then one feed knob per
    /// distinct Grid lane output in lane order (`0x3597ac`, at most four); LEVEL in slot 5,
    /// then each knob's feed meter below it.
    pub(crate) fn cells(self, rows: &[crate::RowView]) -> [Vec<IoCell>; 2] {
        let [mut top, mut bottom] = self.fixed_cells().map(<[IoCell]>::to_vec);
        if self == Self::Headphones {
            let mut feeds = Vec::new();
            for feed in rows.iter().filter_map(|row| crate::HpFeed::of(row.output)) {
                if !feeds.contains(&feed) {
                    feeds.push(feed);
                }
            }
            top.extend((feeds.iter()).map(|&feed| IoCell::Setting(SettingId::HeadphoneFeed(feed))));
            bottom.extend(feeds.into_iter().map(IoCell::FeedMeter));
        }
        [top, bottom]
    }

    /// The cells that do not depend on the Grid. Stock order per port (manual §3,
    /// w4-iosettings §1.1): OUT 3/4 has no ground lift; RET 1/2 have one; LIMITER is a meter.
    fn fixed_cells(self) -> [&'static [IoCell]; 2] {
        use IoCell::*;
        use SettingId as S;
        match self {
            Self::Input1 => [
                &[
                    Setting(S::InputLevel(InputJack::In1)),
                    Setting(S::InputImpedance(InputJack::In1)),
                    Setting(S::InputType(InputJack::In1)),
                    Setting(S::Phantom(InputJack::In1)),
                    Setting(S::InputGroundLift(InputJack::In1)),
                ],
                &[Meter("IN 1 LEVEL")],
            ],
            Self::Input2 => [
                &[
                    Setting(S::InputLevel(InputJack::In2)),
                    Setting(S::InputImpedance(InputJack::In2)),
                    Setting(S::InputType(InputJack::In2)),
                    Setting(S::Phantom(InputJack::In2)),
                    Setting(S::InputGroundLift(InputJack::In2)),
                ],
                &[Meter("IN 2 LEVEL")],
            ],
            Self::Out12 => [
                &[
                    Setting(S::OutputLevel(OutputJack::Out12)),
                    Setting(S::OutputGroundLift(OutputJack::Out12)),
                    Setting(S::OutputMute(OutputJack::Out12)),
                ],
                &[Meter("OUT 1/L"), Meter("OUT 2/R"), Meter("LIMITER")],
            ],
            Self::Out34 => [
                &[
                    Setting(S::OutputLevel(OutputJack::Out34)),
                    Setting(S::OutputMute(OutputJack::Out34)),
                ],
                &[Meter("OUT 3/L"), Meter("OUT 4/R"), Meter("LIMITER")],
            ],
            Self::Send => [
                &[
                    Setting(S::SendLevel(LoopJack::One)),
                    Setting(S::SendLevel(LoopJack::Two)),
                ],
                &[Meter("SEND 1"), Meter("SEND 2"), Meter("LIMITER")],
            ],
            Self::Return => [
                &[
                    Setting(S::ReturnLevel(LoopJack::One)),
                    Setting(S::ReturnGroundLift(LoopJack::One)),
                    Setting(S::ReturnLevel(LoopJack::Two)),
                    Setting(S::ReturnGroundLift(LoopJack::Two)),
                ],
                &[Meter("RET 1"), Meter("RET 2")],
            ],
            Self::Headphones => [&[Setting(S::HeadphoneLevel)], &[HeadphoneMeter]],
            Self::Usb => [
                &[
                    Setting(S::UsbLevel),
                    UsbMeter(
                        "IN 1/2",
                        "USB inputs 1/2 to the host: the dry signal of analog INPUT 1 and INPUT 2.",
                    ),
                    UsbMeter(
                        "IN 3/4",
                        "USB inputs 3/4 to the host: the processed signal of analog OUTPUT 1/L and 2/R.",
                    ),
                    UsbMeter(
                        "IN 5/6",
                        "USB inputs 5/6 to the host: the Grid's USB outputs 5 and 6.",
                    ),
                    UsbMeter(
                        "IN 7/8",
                        "USB inputs 7/8 to the host: the Grid's USB outputs 7 and 8.",
                    ),
                ],
                &[
                    Setting(S::HeadphoneSource),
                    UsbMeter(
                        "OUT 1/2",
                        "USB outputs 1/2 from the host: playback to OUTPUT 1/L and 2/R.",
                    ),
                    UsbMeter(
                        "OUT 3/4",
                        "USB outputs 3/4 from the host: playback to OUTPUT 3/L and 4/R.",
                    ),
                    UsbMeter(
                        "OUT 5/6",
                        "USB outputs 5/6 from the host: the Grid's USB inputs 5 and 6.",
                    ),
                    UsbMeter(
                        "OUT 7/8",
                        "USB outputs 7/8 from the host: the Grid's USB inputs 7 and 8.",
                    ),
                ],
            ],
            Self::UsbRouting => [
                &[
                    Setting(S::UsbRouting),
                    Text(
                        "Determines whether USB Outputs 1/2 or 3/4 carry clean DI signals or processed audio. Channel names on your computer will update after reconnecting or power cycling your device.",
                    ),
                ],
                &[],
            ],
            Self::Expression => [
                &[
                    Text("Please plug in an expression pedal to EXP 1 or 2"),
                    Action(DeviceAction::CalibrateExpression(1), "CALIBRATE EXP 1"),
                    Action(DeviceAction::CalibrateExpression(2), "CALIBRATE EXP 2"),
                ],
                &[],
            ],
            Self::Midi => [&[Setting(S::MidiThru)], &[]],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `0x5cd41c`: pan values are hundredths of the −1..1 range; the magnitude is half of it.
    #[test]
    fn metronome_pan_reads_centre_left_and_right() {
        let pan = |raw| SettingId::MetronomePan.format(Some(&SettingValue::Number(raw)));
        assert_eq!(pan(0), "C");
        assert_eq!(pan(-100), "50 L");
        assert_eq!(pan(51), "26 R");
    }

    #[test]
    fn stepping_wraps_choices_clamps_known_bounds_and_needs_a_current_value() {
        let reference = SettingId::TunerReference;
        assert_eq!(
            reference.step(Some(&SettingValue::Number(480)), 1),
            Some(SettingValue::Number(480))
        );
        assert_eq!(reference.format(Some(&SettingValue::Number(440))), "440 Hz");
        assert_eq!(
            SettingId::HoldTiming.step(Some(&SettingValue::Choice(0)), -1),
            Some(SettingValue::Choice(5))
        );
        assert_eq!(SettingId::MidiThru.step(None, 1), None);
        assert_eq!(
            SettingId::HeadphoneLevel.format(Some(&SettingValue::Number(-35))),
            "-3.5 dB"
        );
        assert_eq!(fixed(-5, 1), "-0.5");
        assert_eq!(
            SettingId::MidiClockOut.format(Some(&SettingValue::Choice(9))),
            "-"
        );
    }

    /// Review R6: `Choice` index = stored stock value (MIDI channel: value − 1).
    #[test]
    fn option_lists_follow_stock_values_and_order() {
        let options = |id: SettingId| match id.kind() {
            SettingKind::Choice(options) => options,
            _ => panic!("{id:?} is not a choice"),
        };
        assert_eq!(
            options(SettingId::PowerButtonSensitivity),
            ["Off", "Low", "Medium", "High"]
        );
        assert_eq!(options(SettingId::MidiChannel)[0], "1");
        assert_eq!(options(SettingId::MidiChannel)[16], "OMNI");
        assert_eq!(
            options(SettingId::MidiClockOut),
            ["OFF", "DIN", "USB", "USB+DIN"]
        );
        // w4-iosettings §1.1: ctor option order is the stored index.
        assert_eq!(
            options(SettingId::HeadphoneSource),
            ["NONE", "USB 1/2", "USB 3/4", "BOTH"]
        );
        assert_eq!(options(SettingId::UsbRouting), ["1/2 - 3/4", "3/4 - 1/2"]);
    }
}

/// Downloads folder of the Excessive Downloads check (`0x22cab0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ExcessDownloads {
    Captures,
    Presets,
}

impl ExcessDownloads {
    /// The `%1` of the dialog texts.
    pub fn label(self) -> &'static str {
        match self {
            Self::Captures => "Captures",
            Self::Presets => "Presets",
        }
    }
}
