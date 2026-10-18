//! `ZenHAL::CZenCommon` product detection, ZenUI platform policy and `CZendisplay`
//! identity, as pure functions.
use crate::requests::Brightness;

/// `getProcCmdlineValue` (VA 0x7a10): the first line of `/proc/cmdline`, split on
/// single spaces; the first token starting with `key=` gives the rest of the token.
/// Stock returns `""` both when the key is missing and when its value is empty.
pub fn cmdline_value<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    let line = cmdline.split('\n').next().unwrap_or_default();
    line.split(' ').find_map(|token| {
        token
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
    })
}

/// The `productid=` kernel argument.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Product {
    /// `isQC` (VA 0x830c): `productid` missing, empty or `quadcortex`.
    QuadCortex,
    /// `isAtma` (VA 0x84d8): `productid=atma`.
    Atma,
    Other(String),
}

impl Product {
    pub fn from_cmdline(cmdline: &str) -> Self {
        match cmdline_value(cmdline, "productid").unwrap_or_default() {
            "" | "quadcortex" => Self::QuadCortex,
            "atma" => Self::Atma,
            other => Self::Other(other.to_owned()),
        }
    }

    /// `isQCwithESS` (VA 0x8498): a QC with zenjack revision 2 and zencoder revision 1.
    /// Pass the stock substitute values on ioctl failure (zenjack 4, zencoder 3).
    pub fn is_qc_with_ess(&self, zenjack_revision: i32, zencoder_revision: i32) -> bool {
        *self == Self::QuadCortex && zenjack_revision == 2 && zencoder_revision == 1
    }
}

/// ZenUI board platform (`DAT_015399c8`, detector 0x004962ac). It drives ZenUI
/// policy (gain math, MIDI UART, power button) and is separate from libzenhal `_hwType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// "Revision: QC", CS4385/CS5364 codec.
    QcOriginal = 0,
    /// "Revision: QC-007", ES9017/ES9826 codec.
    QcEss = 1,
    /// "Revision: ATMA" (Quad Cortex mini).
    Atma = 2,
    /// "Revision: ATMA on QC": mini emulation on QC hardware.
    AtmaEmulation = 3,
}

impl Platform {
    /// Marker files ZenUI tests. The ATMA path is XOR-obfuscated in ZenUI (key `0x36 + i`).
    pub const ATMA_EMULATION_FILE: &str = "/media/p4/atma_emulation";
    pub const ESS_EMULATION_FILE: &str = "/media/p4/ess_emulation";

    /// Test order: `isAtma`, ATMA emulation file, `isQCwithESS` or ESS emulation file.
    pub fn detect(
        product: &Product,
        qc_with_ess: bool,
        atma_emulation_file: bool,
        ess_emulation_file: bool,
    ) -> Self {
        if *product == Product::Atma {
            Self::Atma
        } else if atma_emulation_file {
            Self::AtmaEmulation
        } else if qc_with_ess || ess_emulation_file {
            Self::QcEss
        } else {
            Self::QcOriginal
        }
    }

    /// 0x0049a754: full-size QC.
    pub fn is_qc(self) -> bool {
        matches!(self, Self::QcOriginal | Self::QcEss)
    }

    /// 0x00499a60: mini UI and policy.
    pub fn is_atma(self) -> bool {
        matches!(self, Self::Atma | Self::AtmaEmulation)
    }

    /// 0x00497fd8: ES9017/ES9826 codec; ZenUI then calls `CodecOpen`.
    pub fn has_ess_codec(self) -> bool {
        self != Self::QcOriginal
    }

    /// 0x0049c14c: `CMIDI::Open(true)` opens `/dev/ttySC1`; real ATMA routes MIDI via the DSP.
    pub fn uses_midi_uart(self) -> bool {
        self != Self::Atma
    }

    /// Power-button thread 0x006a80d4: ATMA reports press as 1, QC as 0 (release edge).
    pub fn power_button_pressed(self, value: i32) -> bool {
        if self.is_atma() {
            value == 1
        } else {
            value == 0
        }
    }
}

/// `displayinfo_t` from `/tmp/qc_display` (`CZendisplay::Open` VA 0x8a38 reads the
/// file and drops its last byte; `DisplayInfoGet` VA 0x9454 compares the rest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayPanel {
    WinOld = 1,
    Fvd = 2,
    WinNew = 3,
    Atma = 4,
}

impl DisplayPanel {
    /// `None` is stock `displayinfo_t` 0 with a false return. Stock throws
    /// `out_of_range` on an empty file; Rust returns `None`.
    pub fn from_file(contents: &[u8]) -> Option<Self> {
        let (_, name) = contents.split_last()?;
        match name {
            b"win-old" => Some(Self::WinOld),
            b"fvd" => Some(Self::Fvd),
            b"win-new" => Some(Self::WinNew),
            b"atma" => Some(Self::Atma),
            _ => None,
        }
    }
}

impl DisplayPanel {
    /// ZenUI backlight (0x00409da4, divisors from 0x0040a07c): percent clamped
    /// to 4..=100, divided by the panel factor in f32, truncated, at least 1.
    pub fn backlight(panel: Option<Self>, percent: i32) -> Brightness {
        let divisor = match panel {
            Some(Self::Fvd) => 9.0,
            Some(Self::WinNew) => 6.0,
            Some(Self::Atma) => 2.9,
            Some(Self::WinOld) | None => 1.0f32,
        };
        let percent = percent.clamp(4, 100);
        let level = ((percent as f32 / divisor) as i32).max(1);
        Brightness::try_from(level).expect("level is within 1..=100")
    }
}

/// `touchinfo_t`: the word `/dev/touchinfo` returns for request `0x80046162`
/// (`_IOR('a', 0x62, int)`) at open. `TouchInfoGet` (VA 0x9414) accepts only 0/1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TouchController {
    Kind0 = 0,
    Kind1 = 1,
}

impl TouchController {
    pub const REQUEST: u32 = 0x8004_6162;

    pub fn from_word(word: i32) -> Option<Self> {
        match word {
            0 => Some(Self::Kind0),
            1 => Some(Self::Kind1),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmdline_and_product_follow_stock_token_rules() {
        let line = "console=ttySC0 productid=atma  quiet\nproductid=quadcortex";
        assert_eq!(cmdline_value(line, "productid"), Some("atma"));
        assert_eq!(Product::from_cmdline(line), Product::Atma);
        assert_eq!(
            Product::from_cmdline("root=/dev/mmcblk0p2"),
            Product::QuadCortex
        );
        assert_eq!(Product::from_cmdline("productid="), Product::QuadCortex);
        // Prefix match needs the `=`: `productidx=1` is a different key.
        assert_eq!(cmdline_value("productidx=1", "productid"), None);
        assert_eq!(
            Product::from_cmdline("productid=nano"),
            Product::Other("nano".into())
        );
        assert!(Product::QuadCortex.is_qc_with_ess(2, 1));
        assert!(!Product::Atma.is_qc_with_ess(2, 1));
    }

    #[test]
    fn platform_detection_and_policies() {
        let qc = Product::QuadCortex;
        assert_eq!(
            Platform::detect(&Product::Atma, true, true, true),
            Platform::Atma
        );
        assert_eq!(
            Platform::detect(&qc, true, true, false),
            Platform::AtmaEmulation
        );
        assert_eq!(Platform::detect(&qc, false, false, true), Platform::QcEss);
        assert_eq!(
            Platform::detect(&qc, false, false, false),
            Platform::QcOriginal
        );
        assert!(Platform::QcOriginal.power_button_pressed(0));
        assert!(!Platform::QcEss.power_button_pressed(1));
        assert!(Platform::AtmaEmulation.power_button_pressed(1));
        assert!(Platform::AtmaEmulation.uses_midi_uart());
        assert!(!Platform::Atma.uses_midi_uart());
        let level = |panel, pct| DisplayPanel::backlight(panel, pct).percent();
        assert_eq!(level(None, 50), 50);
        assert_eq!(level(Some(DisplayPanel::Fvd), 50), 5);
        assert_eq!(level(Some(DisplayPanel::Atma), 100), 34);
        assert_eq!(level(Some(DisplayPanel::Fvd), 0), 1);
    }

    #[test]
    fn display_file_drops_exactly_one_trailing_byte() {
        assert_eq!(
            DisplayPanel::from_file(b"win-new\n"),
            Some(DisplayPanel::WinNew)
        );
        assert_eq!(DisplayPanel::from_file(b"fvd"), None);
        assert_eq!(DisplayPanel::from_file(b"atma\n\n"), None);
        assert_eq!(DisplayPanel::from_file(b""), None);
    }
}
