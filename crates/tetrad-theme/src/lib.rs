//! Colour themes of the touch UI: the [`Rgb`] colour, the [`Palette`] of chrome roles with
//! the stock look as [`Palette::STOCK`], the built-in themes, and [`ThemeSpec`], the schema of
//! theme files and extension themes.
//!
//! Only chrome is themed. Block category colours, scene swatches, LEDs and the tuner's
//! in-tune green are data and stay fixed (`tetrad-assets`).
#![forbid(unsafe_code)]

mod builtin;
mod spec;
mod tone;

use serde::Deserialize;

pub use builtin::{BUILTINS, Builtin, builtin};
pub use spec::{MAX_ID, MAX_NAME, ThemeError, ThemeId, ThemeSpec, check, contrast};

/// Opaque RGB colour. Theme files write it as `#rrggbb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
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

    /// `0xrrggbb`.
    pub const fn hex(value: u32) -> Rgb {
        Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }
}

impl TryFrom<String> for Rgb {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        text.strip_prefix('#')
            .filter(|hex| hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .map(Rgb::hex)
            .ok_or_else(|| format!("{text:?} is not a #rrggbb colour"))
    }
}

impl std::fmt::Display for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

/// Defines [`Palette`], [`Palette::STOCK`] and [`Colors`] from one role list, so the three
/// cannot drift apart.
macro_rules! palette {
    ($($(#[$doc:meta])* $role:ident = $stock:literal,)*) => {
        /// The chrome colours of one theme. Screens read them from their `Layout`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Palette {
            $($(#[$doc])* pub $role: Rgb,)*
        }

        impl Palette {
            /// The stock CorOS look (stock `QColor(QRgb)` constants, tokens §2). A 16 bpp
            /// framebuffer truncates them the same way the stock screenshots show.
            pub const STOCK: Palette = Palette { $($role: Rgb::hex($stock),)* };
        }

        /// The roles a theme sets, by their camelCase names. Missing roles come from the
        /// theme's base.
        #[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct Colors {
            $(#[serde(default)] pub $role: Option<Rgb>,)*
        }

        impl Colors {
            fn apply(&self, palette: &mut Palette) {
                $(if let Some(color) = self.$role {
                    palette.$role = color;
                })*
            }
        }
    };
}

palette! {
    /// Page background and inset controls on cards: dropdown fields, bar tracks, black
    /// buttons (ASSIGN, UNMUTE), `+` slots. Also the divider on cards.
    background = 0x121212,
    /// Grid, tuner and tempo background; block tile fill; knob face; popover menus.
    canvas = 0x000000,
    /// Cards, menu lists, parameter panels and the top-right ✓ button.
    panel = 0x2e2e2e,
    /// Selected menu row, top-bar icon buttons, the picker drawer, unselected tabs.
    surface = 0x1e1e1e,
    /// Selected preset row; knob track ring.
    row_selected = 0x252525,
    /// Search section bars; idle tempo beat dots.
    section = 0x404040,
    /// 1–2 px separators between cells and rows on cards.
    divider = 0x121212,
    /// Scroll thumb (6 px wide).
    scrollbar = 0x373737,
    /// Primary text.
    text = 0xffffff,
    /// Secondary text: page titles, units, info icons.
    muted = 0x959595,
    /// Unselected options, ⋮ dots and disabled text.
    faint = 0x616161,
    /// Grid cable (2 px).
    cable = 0xc1c1c1,
    /// Selected text, switch rings, bars, knob arcs, BPM, meters.
    accent = 0x45f862,
    /// Selected tab fill under a 1 px accent outline.
    accent_fill = 0x17291a,
    /// Text on accent and warning fills.
    on_accent = 0x000000,
    /// Primary action fill (Save, START CAPTURE).
    action = 0x1a3af8,
    /// Record, MUTED, errors.
    danger = 0xff2727,
    /// Scene letter badge and "New" badge.
    warning = 0xffd236,
}

impl Default for Palette {
    fn default() -> Self {
        Self::STOCK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_endpoints_are_exact() {
        let stock = Palette::STOCK;
        assert_eq!(stock.accent.mix(stock.background, 0), stock.accent);
        assert_eq!(stock.accent.mix(stock.background, 255), stock.background);
        assert_eq!(
            Rgb(0, 0, 0).mix(Rgb(255, 255, 255), 128),
            Rgb(128, 128, 128)
        );
    }

    #[test]
    fn hex_text_round_trips() {
        assert_eq!(
            Rgb::try_from("#17291a".to_owned()),
            Ok(Rgb(0x17, 0x29, 0x1a))
        );
        assert_eq!(Rgb(0x17, 0x29, 0x1a).to_string(), "#17291a");
        for bad in ["17291a", "#17291", "#17291az", "#ggggggg", "#+1291a"] {
            assert!(Rgb::try_from(bad.to_owned()).is_err(), "{bad}");
        }
    }
}
