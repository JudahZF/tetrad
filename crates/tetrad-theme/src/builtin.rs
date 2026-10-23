//! The themes that ship with Tetrad. All are dark: the two-tone block glyphs only read right
//! on a dark background.

use crate::{Palette, Rgb};

/// A theme compiled into Tetrad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Builtin {
    pub id: &'static str,
    pub name: &'static str,
    pub palette: Palette,
}

/// Every built-in theme, Stock first.
pub const BUILTINS: [Builtin; 4] = [
    Builtin {
        id: "stock",
        name: "Stock",
        palette: Palette::STOCK,
    },
    Builtin {
        id: "stage-contrast",
        name: "Stage High Contrast",
        palette: Palette {
            background: Rgb::hex(0x101010),
            canvas: Rgb::hex(0x000000),
            panel: Rgb::hex(0x2a2a2a),
            surface: Rgb::hex(0x1a1a1a),
            row_selected: Rgb::hex(0x242424),
            section: Rgb::hex(0x3a3a3a),
            divider: Rgb::hex(0x101010),
            scrollbar: Rgb::hex(0x4a4a4a),
            text: Rgb::hex(0xffffff),
            muted: Rgb::hex(0xc8c8c8),
            faint: Rgb::hex(0x8c8c8c),
            cable: Rgb::hex(0xffffff),
            accent: Rgb::hex(0xffe100),
            accent_fill: Rgb::hex(0x1a1700),
            on_accent: Rgb::hex(0x000000),
            action: Rgb::hex(0x2f6bff),
            danger: Rgb::hex(0xff3b3b),
            warning: Rgb::hex(0xff9f1a),
        },
    },
    Builtin {
        id: "slate",
        name: "Slate",
        palette: Palette {
            background: Rgb::hex(0x10151b),
            canvas: Rgb::hex(0x090c10),
            panel: Rgb::hex(0x26303b),
            surface: Rgb::hex(0x1a222b),
            row_selected: Rgb::hex(0x202932),
            section: Rgb::hex(0x354251),
            divider: Rgb::hex(0x10151b),
            scrollbar: Rgb::hex(0x2f3a46),
            text: Rgb::hex(0xeef3f8),
            muted: Rgb::hex(0x9aa8b6),
            faint: Rgb::hex(0x627080),
            cable: Rgb::hex(0xb8c4d0),
            accent: Rgb::hex(0x5aa9ff),
            accent_fill: Rgb::hex(0x14243a),
            on_accent: Rgb::hex(0x000000),
            action: Rgb::hex(0x3b5bdb),
            danger: Rgb::hex(0xff5c5c),
            warning: Rgb::hex(0xffd166),
        },
    },
    Builtin {
        id: "ember",
        name: "Ember",
        palette: Palette {
            background: Rgb::hex(0x15110e),
            canvas: Rgb::hex(0x0b0806),
            panel: Rgb::hex(0x2f2621),
            surface: Rgb::hex(0x211a16),
            row_selected: Rgb::hex(0x29211c),
            section: Rgb::hex(0x42362f),
            divider: Rgb::hex(0x15110e),
            scrollbar: Rgb::hex(0x3a2f28),
            text: Rgb::hex(0xfff6ee),
            muted: Rgb::hex(0xb8a596),
            faint: Rgb::hex(0x75665b),
            cable: Rgb::hex(0xd6c4b5),
            accent: Rgb::hex(0xffa53a),
            accent_fill: Rgb::hex(0x2e1f10),
            on_accent: Rgb::hex(0x000000),
            action: Rgb::hex(0xc4471c),
            danger: Rgb::hex(0xff4d4d),
            warning: Rgb::hex(0xffd23f),
        },
    },
];

/// The built-in theme `id`.
pub fn builtin(id: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|theme| theme.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A colour tweak must not make a built-in theme unreadable on stage.
    #[test]
    fn every_builtin_passes_the_checks() {
        for theme in &BUILTINS {
            assert_eq!(crate::check(&theme.palette), Ok(()), "{}", theme.id);
            assert!(crate::spec::valid_id(theme.id), "{}", theme.id);
        }
    }
}
