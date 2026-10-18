//! I/O SETTINGS page (`[ConnectorsSettings]`, output pairing, selected
//! connector), expression pedal calibration (`[ExpressionsSettings]`) and
//! the Global EQ (`[GlobalEq]`).

use std::collections::BTreeMap;

use crate::Variant;
use crate::device::Hardware;
use crate::typed::{KeyReader, KeyWriter, LoadIssueKind, SettingError};

/// Stored I/O parameters (`ConnectorsSettings/<id>`, normalized 0..1) with their stock ids.
/// QC table `0x51cc4..0x57940`, mini (ATMA) table `0x59ea8..0x5ea3c`, one ctor call
/// `0x5cea30` each. Meters (ids 1710..1748) are never stored and are not listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum ConnectorId {
    In1Level = 100,
    In1Impedance = 101,
    In1Type = 102,
    In1Phantom = 104,
    In1GroundLift = 108,
    In2Level = 200,
    In2Impedance = 201,
    In2Type = 202,
    In2Phantom = 204,
    In2GroundLift = 208,
    Send1Level = 300,
    Send2Level = 400,
    Ret1Level = 500,
    Ret1GroundLift = 502,
    Ret2Level = 600,
    Ret2GroundLift = 602,
    Out3Level = 700,
    Out3Mute = 701,
    Out34Level = 750,
    Out34Mute = 751,
    Out4Level = 800,
    Out4Mute = 801,
    HpLevel = 900,
    Out1Level = 1000,
    Out1GroundLift = 1001,
    Out1Mute = 1002,
    Out12Level = 1050,
    Out12GroundLift = 1051,
    Out12Mute = 1052,
    Out2Level = 1100,
    Out2GroundLift = 1101,
    Out2Mute = 1102,
    MidiThru = 1301,
    Exp1Position = 1402,
    Exp2Position = 1502,
    UsbLevel = 1600,
    HpSource = 1601,
    /// "DRY - WET": USB 1/2 - 3/4 or 3/4 - 1/2.
    UsbDryWet = 1602,
    HpFeedOut1 = 1700,
    HpFeedOut2 = 1701,
    HpFeedOut3 = 1702,
    HpFeedOut4 = 1703,
    HpFeedSend1 = 1704,
    HpFeedSend2 = 1705,
    HpFeedOut12 = 1706,
    /// Mirrors [`Self::HpFeedOut12`] both ways (`0x35476c`); both drive mix 1.
    HpFeedMultiOut = 1707,
    HpFeedOut34 = 1708,
    HpFeedSend12 = 1709,
}

/// Value shape of one connector parameter (ctor kind 0 level, 3 toggle, 4 choice).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConnectorKind {
    /// Linear in dB (skew 1.0): `dB = min + v·(max − min)`; `steps` positions.
    Level {
        min_db: f32,
        max_db: f32,
        steps: u16,
    },
    /// EXP POSITION: 0..1, steps −1 (continuous), no apply path found.
    Position,
    Toggle(&'static [&'static str]),
    Choice(&'static [&'static str]),
}

/// Ctor arguments of one parameter on one hardware. `default` is normalized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConnectorSpec {
    pub kind: ConnectorKind,
    pub default: f32,
}

const OFF_ON: &[&str] = &["Off", "On"];
const IMPEDANCES: &[&str] = &[
    "10k", "33k", "68k", "91k", "130k", "232k", "470k", "1M", "10M",
];
/// −40..12 dB in 209 steps: outputs, sends, HP level and HP feeds.
const OUTPUT_LEVEL: ConnectorKind = ConnectorKind::Level {
    min_db: -40.0,
    max_db: 12.0,
    steps: 209,
};

impl ConnectorId {
    /// Stock stores only ids below this (`0x6ae`, save-all `0x353fa0`).
    pub const STORED_LIMIT: u16 = 1710;

    /// QC table order: the boot restore applies in this order (`0x35e5cc`).
    pub const QC_TABLE: [Self; 48] = {
        use ConnectorId::*;
        [
            In1Level,
            In1Impedance,
            In1Type,
            In1Phantom,
            In1GroundLift,
            In2Level,
            In2Impedance,
            In2Type,
            In2Phantom,
            In2GroundLift,
            Send1Level,
            Send2Level,
            Ret1Level,
            Ret1GroundLift,
            Ret2Level,
            Ret2GroundLift,
            Out3Level,
            Out3Mute,
            Out4Level,
            Out4Mute,
            Out34Level,
            Out34Mute,
            HpLevel,
            HpFeedOut1,
            HpFeedOut2,
            HpFeedOut3,
            HpFeedOut4,
            HpFeedSend1,
            HpFeedSend2,
            HpFeedOut12,
            HpFeedMultiOut,
            HpFeedOut34,
            HpFeedSend12,
            Out1Level,
            Out1GroundLift,
            Out1Mute,
            Out2Level,
            Out2GroundLift,
            Out2Mute,
            Out12Level,
            Out12GroundLift,
            Out12Mute,
            MidiThru,
            Exp1Position,
            Exp2Position,
            UsbLevel,
            HpSource,
            UsbDryWet,
        ]
    };

    /// The parameters of one hardware in table order. The mini table has no impedance,
    /// TYPE, IN 1 phantom, IN 1 lift or return lifts (C); its order is assumed to be the
    /// QC order (I).
    pub fn table(hardware: Hardware) -> impl Iterator<Item = Self> {
        Self::QC_TABLE
            .into_iter()
            .filter(move |id| id.spec(hardware).is_some())
    }

    pub const fn id(self) -> u16 {
        self as u16
    }

    pub fn from_id(id: u16) -> Option<Self> {
        Self::QC_TABLE.into_iter().find(|known| known.id() == id)
    }

    /// The ctor arguments, or `None` when the hardware has no such parameter. ATMA levels
    /// of IN and RET use the mini patch `0x35db28`: −12..30 dB, 169 steps, default 12/42.
    pub fn spec(self, hardware: Hardware) -> Option<ConnectorSpec> {
        use ConnectorId::*;
        let atma = hardware == Hardware::Atma;
        let spec = |kind, default| Some(ConnectorSpec { kind, default });
        match self {
            In1Impedance | In1Type | In1Phantom | In1GroundLift | In2Impedance | In2Type
            | Ret1GroundLift | Ret2GroundLift
                if atma =>
            {
                None
            }
            In1Level | In2Level | Ret1Level | Ret2Level if atma => spec(
                ConnectorKind::Level {
                    min_db: -12.0,
                    max_db: 30.0,
                    steps: 169,
                },
                12.0 / 42.0,
            ),
            In1Level | In2Level | Ret1Level | Ret2Level => spec(
                ConnectorKind::Level {
                    min_db: -12.0,
                    max_db: 60.0,
                    steps: 217,
                },
                1.0 / 6.0,
            ),
            In1Impedance | In2Impedance => spec(ConnectorKind::Choice(IMPEDANCES), 0.875),
            In1Type | In2Type => spec(ConnectorKind::Toggle(&["Instrument", "Mic"]), 0.0),
            In1Phantom | In1GroundLift | In2Phantom | In2GroundLift | Ret1GroundLift
            | Ret2GroundLift | Out1GroundLift | Out2GroundLift | Out12GroundLift | Out1Mute
            | Out2Mute | Out12Mute | Out3Mute | Out4Mute | Out34Mute | MidiThru => {
                spec(ConnectorKind::Toggle(OFF_ON), 0.0)
            }
            Send1Level | Send2Level | Out1Level | Out2Level | Out12Level | Out3Level
            | Out4Level | Out34Level | HpLevel => spec(OUTPUT_LEVEL, 40.0 / 52.0),
            HpFeedOut1 | HpFeedOut2 | HpFeedOut3 | HpFeedOut4 | HpFeedSend1 | HpFeedSend2
            | HpFeedOut12 | HpFeedMultiOut | HpFeedOut34 | HpFeedSend12 => {
                spec(OUTPUT_LEVEL, 46.0 / 52.0)
            }
            Exp1Position | Exp2Position => spec(ConnectorKind::Position, 0.0),
            UsbLevel => spec(
                ConnectorKind::Level {
                    min_db: -40.0,
                    max_db: 0.0,
                    steps: 161,
                },
                1.0,
            ),
            HpSource => spec(
                ConnectorKind::Choice(&["NONE", "USB 1/2", "USB 3/4", "BOTH"]),
                1.0,
            ),
            UsbDryWet => spec(ConnectorKind::Toggle(&["1/2 - 3/4", "3/4 - 1/2"]), 0.0),
        }
    }

    /// The Cortex Control output port of a headphone feed (`OutputPort` numbering,
    /// `0x17bbb4`); the DSP `0x11` mix selector is the same number except MULTI OUT,
    /// which drives mix 1 with OUT 1/2.
    pub fn hp_feed_port(self) -> Option<u8> {
        use ConnectorId::*;
        Some(match self {
            HpFeedOut1 => 4,
            HpFeedOut2 => 5,
            HpFeedOut3 => 6,
            HpFeedOut4 => 7,
            HpFeedSend1 => 8,
            HpFeedSend2 => 9,
            HpFeedOut12 => 1,
            HpFeedMultiOut => 19,
            HpFeedOut34 => 2,
            HpFeedSend12 => 3,
            _ => return None,
        })
    }
}

impl ConnectorSpec {
    /// Option index of a toggle or choice: `round(v·(n − 1))`, clamped. Stock toggle
    /// rounding (`0x5cc2d4`) is not read; rounding is inferred.
    pub fn index(self, value: f32) -> Option<usize> {
        let (ConnectorKind::Toggle(options) | ConnectorKind::Choice(options)) = self.kind else {
            return None;
        };
        let last = options.len() - 1;
        Some(((value.clamp(0.0, 1.0) * last as f32).round() as usize).min(last))
    }

    /// The normalized value of option `index`.
    pub fn index_value(self, index: usize) -> Option<f32> {
        let (ConnectorKind::Toggle(options) | ConnectorKind::Choice(options)) = self.kind else {
            return None;
        };
        (index < options.len()).then(|| index as f32 / (options.len() - 1) as f32)
    }

    /// Display dB of a level: `min + v·(max − min)` (QC IN `v·72 − 12`, mini `v·42 − 12`,
    /// outputs `52v − 40`, USB `40v − 40`).
    pub fn decibels(self, value: f32) -> Option<f32> {
        let ConnectorKind::Level { min_db, max_db, .. } = self.kind else {
            return None;
        };
        Some(value.mul_add(max_db - min_db, min_db))
    }

    /// The normalized value of a dB level, clamped and snapped to the stock steps.
    pub fn from_decibels(self, db: f32) -> Option<f32> {
        let ConnectorKind::Level {
            min_db,
            max_db,
            steps,
        } = self.kind
        else {
            return None;
        };
        let last = f32::from(steps - 1);
        let v = ((db - min_db) / (max_db - min_db)).clamp(0.0, 1.0);
        Some((v * last).round() / last)
    }
}

/// OUT, SEND, HP feed and ESS HP level gain (`0x35476c`, `0x7d6500`):
/// `10^((52v − 40)/20)`, exactly 0 at v ≤ 0 (the UI shows "OFF").
pub fn output_gain(value: f32) -> f32 {
    if value <= 0.0 {
        0.0
    } else {
        10_f32.powf((value * 52.0 - 40.0) / 20.0)
    }
}

/// USB level gain (`0x35476c`, id 1600): `10^((40v − 40)/20)`, 0 at v ≤ 0.
pub fn usb_gain(value: f32) -> f32 {
    if value <= 0.0 {
        0.0
    } else {
        10_f32.powf((value * 40.0 - 40.0) / 20.0)
    }
}

/// The combo-jack input type from TYPE and PHANTOM 48V (`0x7d88ac`; h-hal `input_type_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComboJack {
    Ts,
    Xlr,
    XlrPhantom,
}

impl ComboJack {
    /// TS when TYPE < 0.5, else XLR, or XLR+48V when PHANTOM ≥ 0.5. ESS boards (`0x497fd8`)
    /// ignore TYPE: always XLR or XLR+48V (`0x3594fc`).
    pub fn from_values(type_value: f32, phantom: f32, ess: bool) -> Self {
        if !ess && type_value < 0.5 {
            Self::Ts
        } else if phantom >= 0.5 {
            Self::XlrPhantom
        } else {
            Self::Xlr
        }
    }
}

/// One stored I/O parameter. Keys use the decimal id: `%1`, `exp %1`,
/// `exp min %1`, `exp max %1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConnectorParam {
    /// Normalized value; `None` uses the parameter default (`0x005cc3f8`).
    pub value: Option<f32>,
    /// `exp %1`: 0 = no pedal, otherwise the expression pedal number
    /// (1 or 2, inferred).
    pub expression: i32,
    pub exp_min: f32,
    pub exp_max: f32,
}

impl Default for ConnectorParam {
    fn default() -> Self {
        Self {
            value: None,
            expression: 0,
            exp_min: 0.0,
            exp_max: 1.0,
        }
    }
}

/// I/O SETTINGS state. Connector load `0x0035c4ac` / `0x00360da8`, save
/// all `0x00353fa0`, reset `0x003615a0`, I/O preset load `0x00362d18`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IoSettings {
    /// By parameter id; only ids below [`ConnectorId::STORED_LIMIT`] are
    /// stored.
    pub params: BTreeMap<u16, ConnectorParam>,
    /// `output_0_paired`: XLR OUT 1/L + OUT 2/R as a stereo pair. Boot value
    /// false; the I/O reset sets true.
    pub out12_paired: bool,
    /// `output_1_paired`: OUT 3/L + OUT 4/R.
    pub out34_paired: bool,
    /// `selected_connector`: connector name last selected on the page.
    /// `None` selects the first connector (`0x0035a7f4` save).
    pub selected_connector: Option<String>,
}

const CONNECTORS: &str = "ConnectorsSettings";

impl IoSettings {
    /// The stored value or the parameter default (`0x005cc3f8`); 0 for a parameter the
    /// hardware does not have. A value outside 0..=1 (or NaN) uses the default (owned: stock
    /// takes any float).
    pub fn value(&self, id: ConnectorId, hardware: Hardware) -> f32 {
        self.params
            .get(&id.id())
            .and_then(|param| param.value)
            .filter(|value| (0.0..=1.0).contains(value))
            .or_else(|| id.spec(hardware).map(|spec| spec.default))
            .unwrap_or(0.0)
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let mut io = Self {
            out12_paired: r.bool("output_0_paired", false),
            out34_paired: r.bool("output_1_paired", false),
            selected_connector: Some(r.string("selected_connector", "")).filter(|s| !s.is_empty()),
            ..Self::default()
        };
        for key in r.file.child_keys(CONNECTORS) {
            let (field, id) = match key.rsplit_once(' ') {
                Some((field, id)) => (field, id),
                None => ("", key.as_str()),
            };
            let Ok(id) = id.parse::<u16>() else { continue };
            if !matches!(field, "" | "exp" | "exp min" | "exp max") {
                continue;
            }
            let full = format!("{CONNECTORS}/{key}");
            if id >= ConnectorId::STORED_LIMIT {
                r.issue(&full, LoadIssueKind::NotConvertible);
                continue;
            }
            let param = io.params.entry(id).or_default();
            match field {
                "" => {
                    let value = r.f32(&full, 0.0);
                    param.value = if (0.0..=1.0).contains(&value) {
                        Some(value)
                    } else {
                        let range = SettingError::OutOfRange {
                            name: "connector value",
                            value: f64::from(value),
                            min: 0.0,
                            max: 1.0,
                        };
                        r.issue(&full, LoadIssueKind::Invalid(range));
                        None
                    };
                }
                "exp" => param.expression = r.int(&full, 0),
                "exp min" => param.exp_min = r.f32(&full, 0.0),
                _ => param.exp_max = r.f32(&full, 1.0),
            }
        }
        io
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.bool("output_0_paired", self.out12_paired, old.out12_paired);
        w.bool("output_1_paired", self.out34_paired, old.out34_paired);
        w.put(
            "selected_connector",
            &self.selected_connector,
            &old.selected_connector,
            |s| Variant::String(s.clone().unwrap_or_default()),
        );
        let none = ConnectorParam::default();
        for (&id, param) in self
            .params
            .iter()
            .filter(|(id, _)| **id < ConnectorId::STORED_LIMIT)
        {
            let before = old.params.get(&id).unwrap_or(&none);
            if param.value != before.value {
                let key = format!("{CONNECTORS}/{id}");
                match param.value {
                    Some(v) => w.file.set(&key, &Variant::Float(v)),
                    None => w.file.remove(&key),
                }
            }
            w.int(
                &format!("{CONNECTORS}/exp {id}"),
                param.expression,
                before.expression,
            );
            w.f32(
                &format!("{CONNECTORS}/exp min {id}"),
                param.exp_min,
                before.exp_min,
            );
            w.f32(
                &format!("{CONNECTORS}/exp max {id}"),
                param.exp_max,
                before.exp_max,
            );
        }
        for id in old.params.keys().filter(|id| !self.params.contains_key(id)) {
            for field in ["", "exp ", "exp min ", "exp max "] {
                w.file.remove(&format!("{CONNECTORS}/{field}{id}"));
            }
        }
    }
}

/// Raw ADC range of one expression pedal. Defaults 0 and 255; the I/O
/// reset restores them (`0x00320fdc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpressionRange {
    pub min: i32,
    pub max: i32,
}

impl Default for ExpressionRange {
    fn default() -> Self {
        Self { min: 0, max: 255 }
    }
}

impl ExpressionRange {
    /// `clamp((raw - min) / (max - min), 0, 1)` (`0x00320cf8`). An empty
    /// range gives 0 (inferred; stock divides by zero).
    pub fn normalize(self, raw: i32) -> f32 {
        if self.max <= self.min {
            return 0.0;
        }
        ((raw - self.min) as f32 / (self.max - self.min) as f32).clamp(0.0, 1.0)
    }
}

/// A running calibration (`0x0032103c` start, `0x00320eb8` stop). Each
/// sample widens the range with a 5-count dead zone at both ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpressionCalibrator {
    min: i32,
    max: i32,
}

impl Default for ExpressionCalibrator {
    fn default() -> Self {
        Self {
            min: i32::MAX,
            max: i32::MIN,
        }
    }
}

impl ExpressionCalibrator {
    pub fn sample(&mut self, raw: i32) {
        self.min = self.min.min(raw.saturating_add(5));
        self.max = self.max.max(raw.saturating_sub(5));
    }

    /// The new range, or `None` while either end is still a sentinel
    /// (stock then keeps the old calibration).
    pub fn finish(self) -> Option<ExpressionRange> {
        (self.min != i32::MAX && self.max != i32::MIN).then_some(ExpressionRange {
            min: self.min,
            max: self.max,
        })
    }
}

/// `[ExpressionsSettings]` `expression1Min/Max`, `expression2Min/Max`. Load
/// `0x003207dc`, save `0x00320a34`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExpressionCalibration {
    pub pedals: [ExpressionRange; 2],
}

impl ExpressionCalibration {
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let mut pedals = [ExpressionRange::default(); 2];
        for (i, pedal) in pedals.iter_mut().enumerate() {
            pedal.min = r.int(&format!("ExpressionsSettings/expression{}Min", i + 1), 0);
            pedal.max = r.int(&format!("ExpressionsSettings/expression{}Max", i + 1), 255);
        }
        Self { pedals }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        for (i, (new, old)) in self.pedals.iter().zip(&old.pedals).enumerate() {
            w.int(
                &format!("ExpressionsSettings/expression{}Min", i + 1),
                new.min,
                old.min,
            );
            w.int(
                &format!("ExpressionsSettings/expression{}Max", i + 1),
                new.max,
                old.max,
            );
        }
    }
}

/// `[GlobalEq]` `0/N`: normalized values of the "Parametric-5" global EQ
/// (device model 4004), scene slot 0 only; the reader applies them to all
/// 8 slots. Load `0x002fbf08`, save `0x002fa580`. The saver writes the
/// whole model list: the XML parameters, then MUTE (`0x2ec444`, count − 2)
/// and the block BYPASS (`0x2ecc70`, count − 1), which is the ON button
/// (`0x2ecd4c`: above 0.5 is OFF). The caller takes those indices from the
/// catalog and fills the missing values (a missing BYPASS is OFF).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlobalEq {
    pub params: BTreeMap<u16, f32>,
}

/// Device model of the Global EQ ("Output Equalizer").
pub const GLOBAL_EQ_MODEL: u32 = 4004;
/// Bands of the "Parametric-5" EQ.
pub const GLOBAL_EQ_BANDS: u8 = 5;

/// One model 4004 parameter (ModelRepo 4.1.0: five bands of GAIN, FREQ, Q, TYPE, BYPASS,
/// then OUTPUT and the two hidden ASSIGN_EQ flags). Bands are 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EqParam {
    Gain(u8),
    Freq(u8),
    Q(u8),
    /// Normalized step of 5 (`0x639f34`): PEAK, HI PASS, LO PASS, HI SHELF, LO SHELF.
    Type(u8),
    /// ≥ 0.5: the band runs (`0x2faeac` sends 1.0, else 0.0).
    Bypass(u8),
    Output,
    /// > 0: the curve drives OUT 1/2 (0) or OUT 3/4 (1) (`0x2f9fa8`).
    Assign(u8),
}

impl EqParam {
    /// Parameters `0..=25` reach the DSP; the ASSIGN flags select the outputs.
    pub const DSP_PARAMS: u16 = 26;

    pub const fn index(self) -> u16 {
        let (band, field) = match self {
            Self::Gain(b) => (b, 0),
            Self::Freq(b) => (b, 1),
            Self::Q(b) => (b, 2),
            Self::Type(b) => (b, 3),
            Self::Bypass(b) => (b, 4),
            Self::Output => return 25,
            Self::Assign(output) => return 26 + output as u16,
        };
        band as u16 * 5 + field
    }
}

impl GlobalEq {
    /// A stored value outside `0.0..=1.0` (NaN and infinities included) is reported and left
    /// out, so the catalog default applies. Owned safety: stock `0x2fbf08` sends any float to
    /// DSP `0x3c` (review-w6 F6).
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let mut params = BTreeMap::new();
        for key in r.file.child_keys("GlobalEq/0") {
            if let Ok(index) = key.parse::<u16>() {
                let name = format!("GlobalEq/0/{key}");
                let value = r.f32(&name, 0.0);
                if (0.0..=1.0).contains(&value) {
                    params.insert(index, value);
                } else {
                    let range = SettingError::OutOfRange {
                        name: "Global EQ parameter",
                        value: f64::from(value),
                        min: 0.0,
                        max: 1.0,
                    };
                    r.issue(&name, LoadIssueKind::Invalid(range));
                }
            }
        }
        Self { params }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        for (index, value) in &self.params {
            w.put(
                &format!("GlobalEq/0/{index}"),
                &Some(*value),
                &old.params.get(index).copied(),
                |v| Variant::Float(v.unwrap_or_default()),
            );
        }
        for index in old.params.keys().filter(|i| !self.params.contains_key(i)) {
            w.file.remove(&format!("GlobalEq/0/{index}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recovered tables: 48 stored QC ids, all below the save-all limit; the mini lacks
    /// impedance, TYPE, IN 1 phantom, IN 1 lift and the return lifts. Defaults sit at 0 dB
    /// (HP feeds +6 dB, impedance 1M, HP SOURCE BOTH) on both hardwares.
    #[test]
    fn tables_ids_and_defaults_match_the_ctor_calls() {
        assert!(
            ConnectorId::QC_TABLE
                .iter()
                .all(|id| id.id() < ConnectorId::STORED_LIMIT
                    && ConnectorId::from_id(id.id()) == Some(*id))
        );
        let atma: Vec<_> = ConnectorId::table(Hardware::Atma).collect();
        assert_eq!(ConnectorId::table(Hardware::QuadCortex).count(), 48);
        assert_eq!(atma.len(), 40);
        assert!(atma.contains(&ConnectorId::In2Phantom));
        assert!(!atma.contains(&ConnectorId::In1Phantom));
        for hardware in [Hardware::QuadCortex, Hardware::Atma] {
            let db = |id: ConnectorId| {
                let spec = id.spec(hardware).unwrap();
                spec.decibels(spec.default).unwrap()
            };
            assert!(db(ConnectorId::In1Level).abs() < 1e-5);
            assert!(db(ConnectorId::Out12Level).abs() < 1e-5);
            assert!((db(ConnectorId::HpFeedOut1) - 6.0).abs() < 1e-5);
            assert_eq!(db(ConnectorId::UsbLevel), 0.0);
        }
        let choice = |id: ConnectorId| {
            let spec = id.spec(Hardware::QuadCortex).unwrap();
            spec.index(spec.default)
        };
        assert_eq!(choice(ConnectorId::In1Impedance), Some(7));
        assert_eq!(choice(ConnectorId::HpSource), Some(3));
        assert_eq!(
            IoSettings::default().value(ConnectorId::In1Type, Hardware::Atma),
            0.0
        );
    }

    /// `0x7d6694` (QC 72, mini 42 dB span), `0x35476c` output and USB laws with exact 0 at
    /// v = 0, and step snapping (QC IN 1/3 dB, mini 1/4 dB).
    #[test]
    fn level_laws_and_steps() {
        let qc = ConnectorId::In1Level.spec(Hardware::QuadCortex).unwrap();
        let mini = ConnectorId::In1Level.spec(Hardware::Atma).unwrap();
        assert_eq!(qc.decibels(1.0), Some(60.0));
        assert_eq!(mini.decibels(1.0), Some(30.0));
        assert_eq!(qc.from_decibels(0.1), Some(36.0 / 216.0));
        assert_eq!(mini.from_decibels(0.1), Some(48.0 / 168.0));
        assert_eq!(qc.from_decibels(99.0), Some(1.0));
        assert_eq!(output_gain(0.0), 0.0);
        assert!((output_gain(40.0 / 52.0) - 1.0).abs() < 1e-6);
        assert!((output_gain(1.0) - 10_f32.powf(0.6)).abs() < 1e-5);
        assert_eq!(usb_gain(0.0), 0.0);
        assert_eq!(usb_gain(1.0), 1.0);
        assert!((usb_gain(0.5) - 0.1).abs() < 1e-6);
    }

    /// A stored value outside 0..=1 (NaN, inf, 2.0) loads as the default with an issue, so
    /// it never reaches an output gain or a PGA plan.
    #[test]
    fn out_of_range_stored_values_load_as_the_default() {
        use crate::SettingsFileId;
        use crate::ini::SettingsFile;
        let mut file = SettingsFile::default();
        for (id, value) in [
            (1000, f32::NAN),
            (1100, f32::INFINITY),
            (100, 2.0),
            (200, 0.5),
        ] {
            file.set(&format!("{CONNECTORS}/{id}"), &Variant::Float(value));
        }
        let mut issues = Vec::new();
        let io = IoSettings::load(&mut KeyReader {
            file: &file,
            id: SettingsFileId::Zenjack,
            hardware: Hardware::QuadCortex,
            issues: &mut issues,
        });
        assert_eq!(issues.len(), 3);
        let qc = Hardware::QuadCortex;
        assert_eq!(io.value(ConnectorId::Out1Level, qc), 40.0 / 52.0);
        assert_eq!(io.value(ConnectorId::Out2Level, qc), 40.0 / 52.0);
        assert_eq!(io.value(ConnectorId::In1Level, qc), 1.0 / 6.0);
        assert_eq!(io.value(ConnectorId::In2Level, qc), 0.5);
    }

    /// A corrupt `[GlobalEq]` value never reaches DSP `0x3c`: it is reported and left out, so
    /// the catalog default applies (review-w6 F6).
    #[test]
    fn out_of_range_global_eq_values_are_left_out() {
        use crate::SettingsFileId;
        use crate::ini::SettingsFile;
        let mut file = SettingsFile::default();
        for (index, value) in [(0, f32::NAN), (1, f32::INFINITY), (25, 7.0), (2, 0.25)] {
            file.set(&format!("GlobalEq/0/{index}"), &Variant::Float(value));
        }
        let mut issues = Vec::new();
        let eq = GlobalEq::load(&mut KeyReader {
            file: &file,
            id: SettingsFileId::Zenjack,
            hardware: Hardware::QuadCortex,
            issues: &mut issues,
        });
        assert_eq!(eq.params, BTreeMap::from([(2, 0.25)]));
        assert_eq!(issues.len(), 3);
        assert!((issues.iter()).all(|issue| matches!(issue.kind, LoadIssueKind::Invalid(_))));
    }

    /// `0x7d88ac` and the ESS rule `0x3594fc`: ESS ignores TYPE.
    #[test]
    fn combo_jack_type_and_phantom() {
        use ComboJack::*;
        assert_eq!(ComboJack::from_values(0.0, 1.0, false), Ts);
        assert_eq!(ComboJack::from_values(1.0, 0.0, false), Xlr);
        assert_eq!(ComboJack::from_values(1.0, 1.0, false), XlrPhantom);
        assert_eq!(ComboJack::from_values(0.0, 0.0, true), Xlr);
        assert_eq!(ComboJack::from_values(0.0, 1.0, true), XlrPhantom);
    }
}
