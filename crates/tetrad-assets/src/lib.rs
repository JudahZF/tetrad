//! Visual assets: the fixed data colours (block categories, scene swatches), IBM Plex Sans
//! text (vendored OFL font, rasterized at runtime) and owned line icons. No stock image, icon
//! or font byte from the device is used. Chrome colours are theme roles
//! ([`tetrad_theme::Palette`], re-exported as [`Palette`]).
#![forbid(unsafe_code)]

mod device_art;
mod font;
mod icon;
mod raster;

pub use font::{Glyph, TextSize, Weight, glyph, pen_advance, text_width};
pub use icon::{Icon, IconSize, icon};
pub use raster::Mask;

pub use tetrad_theme::{Palette, Rgb};

// Fixed colours: data, not chrome. Themes do not change them.

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

    /// Spot checks against the recovered jump table (tokens §2.1).
    #[test]
    fn block_colors_follow_the_stock_category_table() {
        assert_eq!(block::category(1), Rgb(0xff, 0x27, 0x27));
        assert_eq!(block::category(27), block::AMP);
        assert_eq!(block::category(15), Rgb(0x45, 0xf8, 0x62));
        assert_eq!(block::category(10), block::category(4));
        assert_eq!(block::category(29), Rgb(0x69, 0x54, 0xff));
        assert_eq!(block::category(17), Rgb(0xff, 0xff, 0xff));
    }
}
