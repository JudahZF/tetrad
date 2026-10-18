//! `ZenHAL::CZencoder`: footswitch/encoder LEDs, tempo LED, volume and board identity.
//! Pure request builders and record decoders; nodes are in [`crate::node`].
use crate::input::InputRecord;
use crate::node::Element;
use crate::requests::{ElementLed, IoctlPayload, IoctlRequest};

/// `/dev/zencoder/fd` and `tempoctrl` request numbers.
pub mod number {
    /// `SetLedColor`/`SetLedColorAtIndex` (VA 0x6698, 0x69e0), five-byte `__led_rgb_cfg`.
    pub const ELEMENT_LED: u32 = 0x4005_e101;
    /// `ReadVolume` (VA 0x6804): one output word.
    pub const READ_VOLUME: u32 = 0x4004_e102;
    /// `AppFwVersion` (VA 0x6a9c).
    pub const APP_FIRMWARE: u32 = 0x4004_e103;
    /// `BootloaderFwVersion` (VA 0x6bb4).
    pub const BOOTLOADER_FIRMWARE: u32 = 0x4004_e105;
    /// `BoarRevisionGet` (VA 0x6ccc).
    pub const BOARD_REVISION: u32 = 0x4004_e108;
    /// `BoardUniqueID` (VA 0x6f90): 16 output bytes.
    pub const UNIQUE_ID: u32 = 0x4010_e109;
    /// `SetTempo` (VA 0x70a4) on `tempoctrl`: one-byte `__tempo_cfg`.
    pub const TEMPO: u32 = 0x4001_e101;
    /// `SetTempoLedColor` (VA 0x7110) on `tempoctrl`.
    pub const TEMPO_LED: u32 = 0x4005_e106;
    /// `SetTempoDefaultLedColor` (VA 0x717c) on `tempoctrl`: NULL argument.
    pub const TEMPO_DEFAULT_LED: u32 = 0x4004_e107;
}

/// `led_color_t` 0..=7. `FillColor` (VA 0x693c) sets bytes 1..=3 of the LED config
/// to 0 or 0xff. The red/green/blue byte order is inferred from the three-bit pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum LedColor {
    Off = 0,
    Red = 1,
    Green = 2,
    Blue = 3,
    Yellow = 4,
    Magenta = 5,
    Cyan = 6,
    White = 7,
}

impl LedColor {
    /// Bytes 1..=3 of `__led_rgb_cfg`.
    pub fn rgb(self) -> [u8; 3] {
        let on = |bit: u8| if self.mask() & bit != 0 { 0xff } else { 0 };
        [on(0b100), on(0b010), on(0b001)]
    }

    fn mask(self) -> u8 {
        match self {
            Self::Off => 0b000,
            Self::Red => 0b100,
            Self::Green => 0b010,
            Self::Blue => 0b001,
            Self::Yellow => 0b110,
            Self::Magenta => 0b101,
            Self::Cyan => 0b011,
            Self::White => 0b111,
        }
    }
}

impl TryFrom<i32> for LedColor {
    type Error = i32;

    /// `SetLedColorAtIndex` rejects color > 7 (VA 0x6a18).
    fn try_from(value: i32) -> Result<Self, i32> {
        Ok(match value {
            0 => Self::Off,
            1 => Self::Red,
            2 => Self::Green,
            3 => Self::Blue,
            4 => Self::Yellow,
            5 => Self::Magenta,
            6 => Self::Cyan,
            7 => Self::White,
            _ => return Err(value),
        })
    }
}

impl ElementLed {
    /// `SetLedColorAtIndex` layout. Stock leaves the last byte uninitialized on the
    /// stack (VA 0x6a04..0x6a5c), so the caller must choose it.
    pub fn palette(element: Element, color: LedColor, opaque: u8) -> Self {
        let [red, green, blue] = color.rgb();
        Self {
            index: element.index(),
            red,
            green,
            blue,
            opaque,
        }
    }

    /// The same five-byte config sent to `tempoctrl` (`SetTempoLedColor`).
    pub fn tempo_request(self) -> IoctlRequest {
        IoctlRequest {
            number: number::TEMPO_LED,
            payload: self.request().payload,
        }
    }
}

/// One-byte `__tempo_cfg`: the tempo LED blink rate in BPM, 0 = off (inferred from
/// ZenUI 0x0051375c/0x00513790, which send 0 to disable the LED).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TempoConfig(pub u8);

impl TempoConfig {
    pub const OFF: Self = Self(0);

    /// ZenUI mapping (0x00513790): 0 -> 120, otherwise at most 255.
    pub fn blink(bpm: u32) -> Self {
        Self(match bpm {
            0 => 120,
            bpm => bpm.min(255) as u8,
        })
    }

    pub fn request(self) -> IoctlRequest {
        IoctlRequest {
            number: number::TEMPO,
            payload: IoctlPayload::Byte([self.0]),
        }
    }
}

pub fn tempo_default_led_request() -> IoctlRequest {
    IoctlRequest {
        number: number::TEMPO_DEFAULT_LED,
        payload: IoctlPayload::Null,
    }
}

/// Record kinds on `/dev/zencoder/volume` (`GetVolumeEvent` VA 0x6710; ZenUI reads the
/// node itself in its volume thread at VA 0x1fdaac and does not import `GetVolumeEvent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumeEvent {
    /// `EV_ABS`/`ABS_X`: the knob position.
    Level(i32),
    /// `EV_KEY` 115/114 (`KEY_VOLUMEUP`/`KEY_VOLUMEDOWN`); stock tests `value == 1`.
    Key { key: VolumeKey, pressed: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumeKey {
    Up,
    Down,
}

impl VolumeEvent {
    pub fn decode(record: InputRecord) -> Option<Self> {
        match (record.event_type, record.code) {
            (3, 0) => Some(Self::Level(record.value)),
            (1, 115) => Some(Self::Key {
                key: VolumeKey::Up,
                pressed: record.value == 1,
            }),
            (1, 114) => Some(Self::Key {
                key: VolumeKey::Down,
                pressed: record.value == 1,
            }),
            _ => None,
        }
    }
}

/// `EV_ABS`/`ABS_X` value of a tempo or expression record (`GetTempoEvent` VA 0x68b0,
/// `GetExpressionEvent` VA 0xc75c). Other records are skipped by stock.
pub fn absolute_value(record: InputRecord) -> Option<i32> {
    (record.event_type == 3 && record.code == 0).then_some(record.value)
}

/// Physical footswitch of each QC element (ZenUI tables at 0xa19238 and the
/// brightness page; letters for 4 and 10 are inferred). Each is also an encoder
/// and has one RGB LED.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Footswitch {
    A,
    B,
    C,
    D,
    BankDown,
    E,
    F,
    G,
    H,
    Tempo,
    BankUp,
}

impl Element {
    pub fn footswitch(self) -> Footswitch {
        use Footswitch::*;
        [A, B, C, D, BankDown, E, F, G, H, Tempo, BankUp][usize::from(self.index())]
    }
}

/// ZenUI LED colour transform (0x00668e98): gamma 2 with green x0.8 and blue x0.7
/// white balance; the brightness goes to the separate fifth byte as `b * b`.
/// Below brightness 0.1875 ZenUI also darkens the colour with `QColor::dark(f)`,
/// `f = (int)fma(powf((0.1875 - b) / 0.1875, 4), 80, 100)`, see `qt_dark`.
pub fn led_output(rgb: [u8; 3], brightness: f32) -> ([u8; 3], u8) {
    let mut rgb16 = rgb.map(|c| u16::from(c) * 257);
    if brightness < 0.1875 {
        let ratio = (0.1875 - brightness) / 0.1875;
        // At least 100 here: the `light` branch of `dark` (factor 1..=99) cannot occur.
        let factor = ratio.powf(4.0).mul_add(80.0, 100.0) as i32;
        if factor >= 100 {
            rgb16 = qt_dark(rgb16, factor);
        }
    }
    let gamma = |channel: u16, weight: f32| {
        let f = f32::from(channel) / QT_CHANNEL * weight;
        let squared = (f64::from(f) * f64::from(f)).min(1.0) as f32;
        (squared * 255.0).round() as u8
    };
    let color = [
        gamma(rgb16[0], 1.0),
        gamma(rgb16[1], 0.8),
        gamma(rgb16[2], 0.7),
    ];
    let squared = brightness * brightness;
    let level = if squared > 1.0 {
        255
    } else {
        match (squared * 255.0) as u8 {
            0 => u8::from(brightness > 0.0),
            level => level,
        }
    };
    (color, level)
}

/// Qt 4.8.7 `QColor` channel scale (`USHRT_MAX`); `qreal` is `f32` in the stock
/// `libQtGui` (`redF` divides with `vdiv.f32`, VA 0x1d444c).
const QT_CHANNEL: f32 = 65535.0;

/// Qt 4 `qRound` for an `f32` `qreal` (inlined in `toHsv` and `toRgb`).
fn q_round(x: f32) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        let floor = (x - 1.0) as i32;
        (x - floor as f32 + 0.5) as i32 + floor
    }
}

/// `QColor` HSV fields (`ct.ahsv`): hue in 1/100 degree (`u16::MAX`: achromatic),
/// saturation and value 0..=65535.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QtHsv {
    hue: u16,
    saturation: u16,
    value: u16,
}

impl QtHsv {
    /// `QColor::toHsv` from RGB (VA 0x1d5ca4). Results are stored with `strh`, so `as u16`.
    fn from_rgb(rgb: [u16; 3]) -> Self {
        let [r, g, b] = rgb.map(|c| f32::from(c) / QT_CHANNEL);
        let max = r.max(g).max(b);
        let delta = max - r.min(g).min(b);
        let value = q_round(max * QT_CHANNEL) as u16;
        if delta.abs() <= 1e-5 {
            return Self {
                hue: u16::MAX,
                saturation: 0,
                value,
            };
        }
        let saturation = q_round(delta / max * QT_CHANNEL) as u16;
        let same = |a: f32, b: f32| (a - b).abs() <= a.abs().min(b.abs()) * 1e-5;
        // The blue case is the remaining one: `max` is one of the three channels.
        let sector = if same(r, max) {
            (g - b) / delta
        } else if same(g, max) {
            (b - r) / delta + 2.0
        } else {
            (r - g) / delta + 4.0
        };
        let degrees = sector * 60.0;
        let hue = if degrees < 0.0 {
            (degrees + 360.0) * 100.0
        } else {
            degrees * 100.0
        };
        Self {
            hue: q_round(hue) as u16,
            saturation,
            value,
        }
    }

    /// `QColor::toRgb` from HSV (VA 0x1d3908, `Hsv` case at 0x1d3afc). `1 - s * f` and
    /// `1 - s * (1 - f)` are fused (`vfms.f32`).
    fn to_rgb(self) -> [u16; 3] {
        if self.saturation == 0 || self.hue == u16::MAX {
            return [self.value; 3];
        }
        let h = if self.hue == 36000 {
            0.0
        } else {
            f32::from(self.hue) / 6000.0
        };
        let s = f32::from(self.saturation) / QT_CHANNEL;
        let v = f32::from(self.value) / QT_CHANNEL;
        let sector = h as i32;
        let f = h - sector as f32;
        let p = (1.0 - s) * v;
        let q = (-s).mul_add(f, 1.0) * v;
        let t = (-s).mul_add(1.0 - f, 1.0) * v;
        let rgb = match sector {
            0 => [v, t, p],
            1 => [q, v, p],
            2 => [p, v, t],
            3 => [p, q, v],
            4 => [t, p, v],
            5 => [v, p, q],
            // Hue above 36000 is not valid; stock keeps the zeroed channels of `invalidate`.
            _ => [0.0; 3],
        };
        rgb.map(|c| q_round(c * QT_CHANNEL) as u16)
    }
}

/// `QColor::dark(factor)` for `factor >= 100` on an RGB colour (VA 0x1d7420): HSV value
/// `value * 100 / factor` (integer division), then back to RGB.
fn qt_dark(rgb: [u16; 3], factor: i32) -> [u16; 3] {
    let mut hsv = QtHsv::from_rgb(rgb);
    hsv.value = (i32::from(hsv.value) * 100 / factor) as u16;
    hsv.to_rgb()
}

/// ZenUI knob acceleration per element (0x00508e68): keep deltas no older than
/// 100 ms, average them, square with the sign of the truncated average, clamp to
/// `±min(3 * count, 20)` and round half away from zero.
#[derive(Debug, Clone, Default)]
pub struct KnobAccelerator {
    history: std::collections::VecDeque<(i64, f32)>,
}

impl KnobAccelerator {
    /// `now_ms` is a monotonic millisecond clock (`QElapsedTimer::elapsed`).
    pub fn push(&mut self, now_ms: i64, delta: i32) -> i32 {
        while let Some(&(at, _)) = self.history.front() {
            if (now_ms.wrapping_sub(at) as u64) < 101 {
                break;
            }
            self.history.pop_front();
        }
        self.history.push_back((now_ms, delta as f32));
        let count = self.history.len() as f32;
        let average = self.history.iter().map(|&(_, d)| d).sum::<f32>() / count;
        // Stock tests the sign after truncation: -1 < average < 0 counts as positive.
        let sign = if (average as i32) < 0 { -1.0 } else { 1.0 };
        let limit = (count * 3.0).min(20.0);
        (average * average * sign).clamp(-limit, limit).round() as i32
    }
}

/// ZenUI master volume normalization (0x001fc518), QC only. Defaults 2..=242
/// (ctor 0x001fd8d0); the factory range comes from the U-Boot environment
/// ([`Self::from_env`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeCalibration {
    pub min: i32,
    pub max: i32,
}

impl Default for VolumeCalibration {
    fn default() -> Self {
        Self { min: 2, max: 242 }
    }
}

/// Why [`VolumeCalibration::from_env`] keeps the defaults. The texts are the stock log
/// lines (qDebug and qWarning in 0x001fcfb4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VolumeCalibrationError {
    /// A variable is missing or empty.
    #[error("Master volume not calibrated")]
    NotCalibrated,
    /// A value is not a base-10 integer in 0..=255, or `max <= min`.
    #[error(
        "SPI flash has invalid  master volume calibration MASTER_VOLUME_MIN \"{min}\" \
         MASTER_VOLUME_MAX \"{max}\""
    )]
    Invalid { min: String, max: String },
}

impl VolumeCalibration {
    /// 0x001fcfb4: the factory range from the `fw_printenv` variables (the map that
    /// 0x0047d5b4 loads; a missing name reads as empty). Each value is parsed like Qt 4
    /// `QString::toInt(&ok, 10)`: surrounding white space, an optional sign, decimal digits.
    /// ZenUI calls it on the QC only (not when 0x00499a60, the mini, is true) and keeps
    /// the defaults on any error.
    pub fn from_env(
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<Self, VolumeCalibrationError> {
        let read = |name: &str| env.get(name).map_or("", String::as_str);
        let (min, max) = (read("MASTER_VOLUME_MIN"), read("MASTER_VOLUME_MAX"));
        if min.is_empty() || max.is_empty() {
            return Err(VolumeCalibrationError::NotCalibrated);
        }
        let byte = |text: &str| {
            text.trim()
                .parse::<i32>()
                .ok()
                .filter(|value| (0..=255).contains(value))
        };
        match (byte(min), byte(max)) {
            (Some(min), Some(max)) if min < max => Ok(Self { min, max }),
            _ => Err(VolumeCalibrationError::Invalid {
                min: min.to_owned(),
                max: max.to_owned(),
            }),
        }
    }

    /// `ReadVolume` word to 0.0..=1.0; at most 0.01 snaps to 0.
    pub fn level(self, raw: i32) -> f32 {
        let (min, max) = (self.min as f32, self.max as f32);
        let mut x = raw as f32;
        if x < min {
            x = min;
        }
        if max < x {
            x = max;
        }
        let level = (x - min) / (max - min);
        if level <= 0.01 { 0.0 } else { level }
    }
}

/// A firmware version word. Stock prints nonzero words with `%x` and reports
/// `"undefined"` for zero or a failed ioctl (VA 0x6a9c, 0xccc8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FirmwareVersion(pub u32);

impl FirmwareVersion {
    pub fn from_word(word: i32) -> Option<Self> {
        (word != 0).then_some(Self(word as u32))
    }
}

impl std::fmt::Display for FirmwareVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

/// Sixteen ID bytes. Stock prints each with `%02X` (VA 0x6f90, 0xd4c8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoardUniqueId(pub [u8; 16]);

impl std::fmt::Display for BoardUniqueId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02X}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_color_table_matches_stock_branches() {
        // Decoded from FillColor VA 0x693c: color -> (byte1, byte2, byte3).
        let expected = [
            (0, [0, 0, 0]),
            (1, [0xff, 0, 0]),
            (2, [0, 0xff, 0]),
            (3, [0, 0, 0xff]),
            (4, [0xff, 0xff, 0]),
            (5, [0xff, 0, 0xff]),
            (6, [0, 0xff, 0xff]),
            (7, [0xff, 0xff, 0xff]),
        ];
        for (value, rgb) in expected {
            assert_eq!(LedColor::try_from(value).unwrap().rgb(), rgb);
        }
        assert_eq!(LedColor::try_from(8), Err(8));
        let led = ElementLed::palette(Element::try_from(10).unwrap(), LedColor::Magenta, 9);
        assert_eq!(
            led.request().payload.bytes(),
            Some([10, 0xff, 0, 0xff, 9].as_slice())
        );
        assert_eq!(led.tempo_request().number, 0x4005_e106);
    }

    #[test]
    fn led_transform_matches_zenui_gamma_and_brightness_byte() {
        // Full brightness: red^2, (0.8 g)^2, (0.7 b)^2 scaled to 255.
        assert_eq!(led_output([255, 255, 255], 1.0), ([255, 163, 125], 255));
        // Default ledBrightness 27%: trunc(0.27^2 * 255) = 18.
        assert_eq!(led_output([255, 0, 0], 0.27), ([255, 0, 0], 18));
        // A positive brightness never yields an off byte; zero does.
        assert_eq!(led_output([0, 0, 0], 0.06).1, 1);
        assert_eq!(led_output([0, 0, 0], 0.0).1, 0);
        // Dimmed 6%: dark factor (int)(0.68^4 * 80 + 100) = 117.
        assert_eq!(led_output([255, 0, 0], 0.06).0, [186, 0, 0]);
    }

    #[test]
    fn led_darkening_round_trips_through_qt4_hsv() {
        // Vectors from the f32 port of libQtGui 4.8.7 `QColor::dark` (no stock capture),
        // cross-checked with an independent Python f32 model: factor 103 gives the 16-bit
        // channels (63380, 12725, 63626); plain 16-bit scaling gives red 238, not 239.
        assert_eq!(led_output([254, 51, 255], 0.1), ([239, 6, 118], 2));
        assert_eq!(led_output([254, 255, 51], 0.1).0, [239, 154, 5]);
    }

    #[test]
    fn volume_range_reads_the_u_boot_variables_like_zenui() {
        use std::collections::BTreeMap;
        let env = |min: &str, max: &str| {
            BTreeMap::from([
                ("MASTER_VOLUME_MIN".to_owned(), min.to_owned()),
                ("MASTER_VOLUME_MAX".to_owned(), max.to_owned()),
            ])
        };
        let calibration = |min, max| VolumeCalibration::from_env(&env(min, max));
        assert_eq!(
            VolumeCalibration::from_env(&BTreeMap::new()),
            Err(VolumeCalibrationError::NotCalibrated)
        );
        assert_eq!(
            calibration("", "200"),
            Err(VolumeCalibrationError::NotCalibrated)
        );
        // `QString::toInt(&ok, 10)`: white space and a sign are accepted.
        assert_eq!(
            calibration(" 10\t", "+250"),
            Ok(VolumeCalibration { min: 10, max: 250 })
        );
        // Out of 0..=255, not decimal, or max <= min: the stock warning and the defaults.
        for (min, max) in [("-1", "200"), ("10", "256"), ("0x10", "200"), ("9", "9")] {
            assert_eq!(
                calibration(min, max),
                Err(VolumeCalibrationError::Invalid {
                    min: min.into(),
                    max: max.into()
                }),
                "{min}..{max}"
            );
        }
        assert_eq!(
            calibration("1", "x").unwrap_err().to_string(),
            "SPI flash has invalid  master volume calibration MASTER_VOLUME_MIN \"1\" \
             MASTER_VOLUME_MAX \"x\""
        );
        assert_eq!(calibration("10", "250").unwrap().level(130), 0.5);
    }

    #[test]
    fn knob_acceleration_window_sign_and_clamp() {
        let mut knob = KnobAccelerator::default();
        assert_eq!(knob.push(0, 1), 1);
        // avg 3 -> 9, clamped to 3 * count = 6.
        assert_eq!(knob.push(10, 5), 6);
        // avg -2/3 truncates to 0, so stock squares it as positive: 0.44 -> 0.
        assert_eq!(knob.push(20, -8), 0);
        // Entries 101 ms or older leave the window.
        assert_eq!(knob.push(121, -3), -3);
    }

    #[test]
    fn volume_and_tempo_follow_zenui() {
        let volume = VolumeCalibration::default();
        assert_eq!(volume.level(0), 0.0);
        assert_eq!(volume.level(4), 0.0); // 2/240 <= 0.01
        assert_eq!(volume.level(122), 0.5);
        assert_eq!(volume.level(1000), 1.0);
        assert_eq!(TempoConfig::blink(0), TempoConfig(120));
        assert_eq!(TempoConfig::blink(300), TempoConfig(255));
        assert_eq!(
            TempoConfig::OFF.request().payload.bytes(),
            Some([0].as_slice())
        );
        assert_eq!(
            Element::try_from(9).unwrap().footswitch(),
            Footswitch::Tempo
        );
    }

    #[test]
    fn identity_formats_match_stock_printf() {
        assert_eq!(FirmwareVersion::from_word(0), None);
        assert_eq!(
            FirmwareVersion::from_word(0x1a2b).unwrap().to_string(),
            "1a2b"
        );
        let mut id = [0u8; 16];
        id[0] = 0xab;
        id[15] = 0x01;
        assert_eq!(
            BoardUniqueId(id).to_string(),
            "AB000000000000000000000000000001"
        );
    }
}
