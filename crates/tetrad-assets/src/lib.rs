//! Visual assets: the stock colour tokens, IBM Plex Sans text (vendored OFL
//! font, rasterized at runtime) and owned line icons. No stock image, icon or
//! font byte from the device is used.
#![forbid(unsafe_code)]

mod device_art;
mod font;
mod icon;
mod raster;

pub use font::{Glyph, TextSize, Weight, glyph, pen_advance, text_width};
pub use icon::{Icon, IconSize, icon};
pub use raster::Mask;

/// Opaque RGB color, independent of stock scene-color storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Linear mix toward `other`; `amount` 0 keeps `self`, 255 gives `other`.
    pub const fn mix(self, other: Rgb, amount: u8) -> Rgb {
        const fn channel(a: u8, b: u8, t: u8) -> u8 {
            ((a as u16 * (255 - t) as u16 + b as u16 * t as u16 + 127) / 255) as u8
        }
        Rgb(
            channel(self.0, other.0, amount),
            channel(self.1, other.1, amount),
            channel(self.2, other.2, amount),
        )
    }

    const fn hex(value: u32) -> Rgb {
        Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }
}

// Stock `QColor(QRgb)` constants (tokens §2). A 16 bpp framebuffer truncates
// them the same way the stock screenshots show.

/// Page background and inset controls on cards: dropdown fields, bar tracks,
/// black buttons (ASSIGN, UNMUTE), `+` slots. Also the divider on cards.
pub const BACKGROUND: Rgb = Rgb::hex(0x121212);
/// Grid, tuner and tempo background; block tile fill; knob face; popover menus.
pub const CANVAS: Rgb = Rgb::hex(0x000000);
/// Cards, menu lists, parameter panels and the top-right ✓ button.
pub const PANEL: Rgb = Rgb::hex(0x2e2e2e);
/// Selected menu row, top-bar icon buttons, the picker drawer, unselected tabs.
pub const SURFACE: Rgb = Rgb::hex(0x1e1e1e);
/// Selected preset row; knob track ring.
pub const ROW_SELECTED: Rgb = Rgb::hex(0x252525);
/// Search section bars; idle tempo beat dots.
pub const SECTION: Rgb = Rgb::hex(0x404040);
/// 1–2 px separators between cells and rows on cards.
pub const DIVIDER: Rgb = BACKGROUND;
pub const TEXT: Rgb = Rgb::hex(0xffffff);
/// Secondary text: page titles, units, info icons.
pub const MUTED: Rgb = Rgb::hex(0x959595);
/// Unselected options, ⋮ dots and disabled text.
pub const FAINT: Rgb = Rgb::hex(0x616161);
/// Selected text, switch rings, bars, knob arcs, BPM.
pub const ACCENT: Rgb = Rgb::hex(0x45f862);
/// Selected tab fill under a 1 px [`ACCENT`] outline.
pub const ACCENT_FILL: Rgb = Rgb::hex(0x17291a);
/// Text on [`ACCENT`] and [`WARNING`] fills.
pub const ON_ACCENT: Rgb = Rgb::hex(0x000000);
/// Primary action fill (Save, START CAPTURE).
pub const ACTION: Rgb = Rgb::hex(0x1a3af8);
/// Record, MUTED, errors.
pub const DANGER: Rgb = Rgb::hex(0xff2727);
/// Scene letter badge and "New" badge.
pub const WARNING: Rgb = Rgb::hex(0xffd236);
/// Scroll thumb (6 px wide).
pub const SCROLLBAR: Rgb = Rgb::hex(0x373737);
/// Grid cable (2 px).
pub const CABLE: Rgb = Rgb::hex(0xc1c1c1);
/// Tuner ball when in tune (`Qt::green`).
pub const TUNER_IN: Rgb = Rgb::hex(0x00ff00);

/// Stock swatch palette in stock order (`0x40b380`). The scene u32 → swatch
/// map is not recovered, so scene colours stay opaque values.
pub const SWATCHES: [Rgb; 11] = [
    Rgb::hex(0xff2727),
    Rgb::hex(0x6954ff),
    Rgb::hex(0xffd236),
    Rgb::hex(0x0a74e0),
    Rgb::hex(0xff7000),
    Rgb::hex(0x00ffdd),
    Rgb::hex(0xff02c2),
    Rgb::hex(0x87daff),
    Rgb::hex(0x45f862),
    Rgb::hex(0x3500f1),
    Rgb::hex(0xffffff),
];

/// Stock block category colours (`0x002eb380`, jump table `0x2eb3b8`).
pub mod block {
    use super::Rgb;

    /// Overdrive (guitar 0, bass 3).
    pub const DRIVE: Rgb = Rgb::hex(0xff7000);
    /// Amps (guitar 1, bass 2) and Looper 27.
    pub const AMP: Rgb = Rgb::hex(0xff2727);
    /// Compressor 5 and Tuner 15.
    pub const DYNAMICS: Rgb = Rgb::hex(0x45f862);
    /// Delay 6 and Reverb 8.
    pub const TIME: Rgb = Rgb::hex(0x00ffdd);
    pub const MODULATION: Rgb = Rgb::hex(0x3500f1);
    /// Wah 9, FX Loop 13, Neural Capture without a sub-type, Utility 16, Morph 26.
    pub const NEUTRAL: Rgb = Rgb::hex(0x959595);
    /// EQ 4 and Splitter 10.
    pub const EQ: Rgb = Rgb::hex(0x0a74e0);
    /// Mixer 11 (string constant).
    pub const MIXER: Rgb = Rgb::hex(0xff0756);
    /// Every cab (12, 21, 32, 33) and the IR Loader 29.
    pub const CAB: Rgb = Rgb::hex(0x6954ff);
    pub const PITCH: Rgb = Rgb::hex(0xffd236);
    pub const FILTER: Rgb = Rgb::hex(0x87daff);
    pub const SYNTH: Rgb = Rgb::hex(0xe44a5d);
    /// Ids without a case (17, 19, 20, 22, 23, 25, 28, 31).
    pub const OTHER: Rgb = Rgb::hex(0xffffff);

    /// Colour for a stock category id (block word `[6]`). Model and Neural
    /// Capture sub-type overrides (tokens §2.1) need data the caller does not
    /// pass yet.
    pub const fn category(id: u8) -> Rgb {
        match id {
            0 | 3 => DRIVE,
            1 | 2 | 27 => AMP,
            5 | 15 => DYNAMICS,
            6 | 8 => TIME,
            7 => MODULATION,
            9 | 13 | 14 | 16 | 26 => NEUTRAL,
            4 | 10 => EQ,
            11 => MIXER,
            12 | 21 | 29 | 32 | 33 => CAB,
            18 => PITCH,
            24 => FILTER,
            30 => SYNTH,
            _ => OTHER,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_endpoints_are_exact() {
        assert_eq!(ACCENT.mix(BACKGROUND, 0), ACCENT);
        assert_eq!(ACCENT.mix(BACKGROUND, 255), BACKGROUND);
        assert_eq!(
            Rgb(0, 0, 0).mix(Rgb(255, 255, 255), 128),
            Rgb(128, 128, 128)
        );
    }

    /// Spot checks against the recovered jump table (tokens §2.1).
    #[test]
    fn block_colors_follow_the_stock_category_table() {
        assert_eq!(block::category(1), Rgb(0xff, 0x27, 0x27));
        assert_eq!(block::category(27), block::AMP);
        assert_eq!(block::category(15), ACCENT);
        assert_eq!(block::category(10), block::category(4));
        assert_eq!(block::category(29), Rgb(0x69, 0x54, 0xff));
        assert_eq!(block::category(17), TEXT);
    }
}
