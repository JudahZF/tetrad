//! Themes from outside Tetrad: [`ThemeSpec`] is what theme files and extension manifests
//! write, [`ThemeSpec::resolve`] turns it into a checked [`Palette`], and [`ThemeId`] names
//! any theme wherever it came from.

use serde::{Deserialize, Serialize};

use crate::{Colors, Palette, Rgb, builtin};

pub const MAX_ID: usize = 32;
/// Longest theme name: it fits a settings row next to the swatches.
pub const MAX_NAME: usize = 24;
/// Largest relative luminance of a dark background (T3 Code's dark threshold).
pub const MAX_LUMINANCE: f32 = 0.18;

/// A role by its theme file name.
type Role = (&'static str, fn(&Palette) -> Rgb);

const TEXT: Role = ("text", |p| p.text);
const MUTED: Role = ("muted", |p| p.muted);
const ACCENT: Role = ("accent", |p| p.accent);
const ON_ACCENT: Role = ("onAccent", |p| p.on_accent);
const DANGER: Role = ("danger", |p| p.danger);

/// Every fill text sits on, with the minimum contrast of `text`, `muted` and `accent` on it.
/// Main text needs 7:1 on the page, cards and canvas; muted labels and the accent as a bare
/// mark (switch rings on section grey) 3:1; the rest is text at 4.5:1.
const FILLS: [(Role, [f32; 3]); 7] = [
    (("background", |p| p.background), [7.0, 4.5, 4.5]),
    (("canvas", |p| p.canvas), [7.0, 3.0, 4.5]),
    (("panel", |p| p.panel), [7.0, 3.0, 4.5]),
    (("surface", |p| p.surface), [4.5, 3.0, 4.5]),
    (("rowSelected", |p| p.row_selected), [4.5, 3.0, 4.5]),
    (("section", |p| p.section), [4.5, 3.0, 3.0]),
    (("accentFill", |p| p.accent_fill), [4.5, 3.0, 4.5]),
];

/// The other pairs the screens draw: button labels on the action fill, Grid cables on the
/// canvas, error text, danger
/// button labels on section grey (2.5:1: stock's own REMOVE reads 2.75:1), and
/// `onAccent`, the ink of every bright fill (accent, badges, the selected paste row, the
/// error card between text and danger, the muted stomp fallback).
const PAIRS: [(Role, Role, f32); 9] = [
    (TEXT, ("action", |p| p.action), 4.5),
    (("cable", |p| p.cable), ("canvas", |p| p.canvas), 3.0),
    (DANGER, ("background", |p| p.background), 4.5),
    (DANGER, ("section", |p| p.section), 2.5),
    (ON_ACCENT, ACCENT, 4.5),
    (ON_ACCENT, ("warning", |p| p.warning), 4.5),
    (ON_ACCENT, MUTED, 4.5),
    (ON_ACCENT, TEXT, 4.5),
    (ON_ACCENT, DANGER, 4.5),
];

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ThemeError {
    #[error("theme ID {0:?} must be 1..={MAX_ID} characters of [a-z0-9-], not starting with -")]
    Id(String),
    #[error("theme name must be 1..={MAX_NAME} characters, with no control characters")]
    Name,
    #[error("base {0:?} is not a built-in theme")]
    Base(String),
    #[error("{0} must be dark: Tetrad themes are dark only")]
    Light(&'static str),
    #[error("{fg} on {bg} has a contrast of {ratio:.1}:1; it needs {min}:1")]
    Contrast {
        fg: &'static str,
        bg: &'static str,
        ratio: f32,
        min: f32,
    },
    #[error("theme file version {0} is not supported")]
    Version(u32),
    #[error("bad theme file: {0}")]
    Json(String),
}

/// A theme as theme files and extension manifests write it.
///
/// ```json
/// { "id": "midnight", "name": "Midnight", "base": "stock",
///   "colors": { "accent": "#7aa2f7", "panel": "#1f2335" } }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeSpec {
    pub id: String,
    pub name: String,
    /// The built-in theme that gives every role `colors` does not set; Stock when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    pub colors: Colors,
}

/// A theme file: a [`ThemeSpec`] with a format version.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ThemeFile {
    version: u32,
    id: String,
    name: String,
    #[serde(default)]
    base: Option<String>,
    colors: Colors,
}

impl ThemeSpec {
    pub const FILE_VERSION: u32 = 1;

    /// Parse a theme file (`{"version": 1, ...}`). Call [`Self::resolve`] to check it.
    pub fn from_file(bytes: &[u8]) -> Result<Self, ThemeError> {
        let file: ThemeFile =
            serde_json::from_slice(bytes).map_err(|e| ThemeError::Json(e.to_string()))?;
        if file.version != Self::FILE_VERSION {
            return Err(ThemeError::Version(file.version));
        }
        Ok(Self {
            id: file.id,
            name: file.name,
            base: file.base,
            colors: file.colors,
        })
    }

    /// The checked palette. Roles `colors` leaves out come from the base, except three that
    /// follow the roles they belong with: `divider` follows `background`, `accentFill` mixes
    /// `background` with `accent` (as stock does, exactly), and `onAccent` is black or white,
    /// whichever reads better on `accent`.
    pub fn resolve(&self) -> Result<Palette, ThemeError> {
        if !valid_id(&self.id) {
            return Err(ThemeError::Id(self.id.clone()));
        }
        let name = &self.name;
        if name.trim().is_empty()
            || name.chars().count() > MAX_NAME
            || name.chars().any(char::is_control)
        {
            return Err(ThemeError::Name);
        }
        let mut palette = match &self.base {
            None => Palette::STOCK,
            Some(base) => {
                builtin(base)
                    .ok_or_else(|| ThemeError::Base(base.clone()))?
                    .palette
            }
        };
        let c = &self.colors;
        c.apply(&mut palette);
        if c.divider.is_none() && c.background.is_some() {
            palette.divider = palette.background;
        }
        if c.accent_fill.is_none() && (c.background.is_some() || c.accent.is_some()) {
            palette.accent_fill = palette.background.mix(palette.accent, 26);
        }
        if c.on_accent.is_none() && c.accent.is_some() {
            palette.on_accent = [Rgb(0, 0, 0), Rgb(255, 255, 255)]
                .into_iter()
                .max_by(|a, b| {
                    contrast(*a, palette.accent).total_cmp(&contrast(*b, palette.accent))
                })
                .unwrap_or(Rgb(0, 0, 0));
        }
        check(&palette)?;
        Ok(palette)
    }
}

pub(crate) fn valid_id(id: &str) -> bool {
    (1..=MAX_ID).contains(&id.len())
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether `palette` is dark and readable, measured as the RGB565 screen shows it.
pub fn check(palette: &Palette) -> Result<(), ThemeError> {
    for (role, color) in [
        ("background", palette.background),
        ("canvas", palette.canvas),
    ] {
        if luminance(color) >= MAX_LUMINANCE {
            return Err(ThemeError::Light(role));
        }
    }
    let inks = (FILLS.iter()).flat_map(|&(fill, mins)| {
        [TEXT, MUTED, ACCENT]
            .into_iter()
            .zip(mins)
            .map(move |(ink, min)| (ink, fill, min))
    });
    for ((fg, fg_of), (bg, bg_of), min) in inks.chain(PAIRS) {
        let ratio = contrast(fg_of(palette), bg_of(palette));
        if ratio < min {
            return Err(ThemeError::Contrast { fg, bg, ratio, min });
        }
    }
    Ok(())
}

/// WCAG contrast ratio of two colours as the RGB565 screen shows them.
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// WCAG relative luminance after RGB565 truncation.
fn luminance(c: Rgb) -> f32 {
    let linear = |v: u8, bits: u32| {
        let kept = v >> (8 - bits);
        let v = f32::from(kept) / ((1u16 << bits) - 1) as f32;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(c.0, 5) + 0.7152 * linear(c.1, 6) + 0.0722 * linear(c.2, 5)
}

/// Names a theme wherever it came from: `stock`, `file:<id>`, `ext:<installation>/<id>`, where
/// the installation is the extension's install key (`owner/repo:extension.id`), so the same
/// extension ID from two repositories names two themes. This is
/// also how `appearance.json` stores the choice.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ThemeId {
    Builtin(String),
    File(String),
    Extension {
        /// The install key, `owner/repo:extension.id`.
        extension: String,
        theme: String,
    },
}

impl ThemeId {
    pub fn stock() -> Self {
        Self::Builtin("stock".into())
    }
}

impl Default for ThemeId {
    fn default() -> Self {
        Self::stock()
    }
}

impl From<String> for ThemeId {
    fn from(text: String) -> Self {
        if let Some(id) = text.strip_prefix("file:") {
            return Self::File(id.into());
        }
        if let Some((extension, theme)) = text.strip_prefix("ext:").and_then(|t| t.rsplit_once('/'))
        {
            return Self::Extension {
                extension: extension.into(),
                theme: theme.into(),
            };
        }
        Self::Builtin(text)
    }
}

impl From<ThemeId> for String {
    fn from(id: ThemeId) -> Self {
        id.to_string()
    }
}

impl std::fmt::Display for ThemeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builtin(id) => f.write_str(id),
            Self::File(id) => write!(f, "file:{id}"),
            Self::Extension { extension, theme } => write!(f, "ext:{extension}/{theme}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(value: serde_json::Value) -> Result<ThemeSpec, serde_json::Error> {
        serde_json::from_value(value)
    }

    /// The schema is a public contract: theme files and extension manifests use it.
    #[test]
    fn resolve_fills_checks_and_rejects() {
        let midnight = spec(json!({
            "id": "midnight", "name": "Midnight",
            "colors": { "background": "#0d1020", "accent": "#7aa2f7" }
        }))
        .unwrap()
        .resolve()
        .unwrap();
        assert_eq!(
            midnight.panel,
            Palette::STOCK.panel,
            "unset roles come from Stock"
        );
        assert_eq!(
            midnight.divider,
            Rgb::hex(0x0d1020),
            "divider follows background"
        );
        assert_eq!(
            midnight.accent_fill,
            Rgb::hex(0x0d1020).mix(Rgb::hex(0x7aa2f7), 26)
        );
        assert_eq!(midnight.on_accent, Rgb(0, 0, 0));
        // The stock accent fill is that same mix.
        let stock = Palette::STOCK;
        assert_eq!(stock.background.mix(stock.accent, 26), stock.accent_fill);

        let based = spec(json!({ "id": "x", "name": "X", "base": "slate", "colors": {} }));
        assert_eq!(
            based.unwrap().resolve().unwrap(),
            builtin("slate").unwrap().palette
        );

        for (value, error) in [
            (
                json!({ "id": "Bad", "name": "X", "colors": {} }),
                "theme ID",
            ),
            (json!({ "id": "-x", "name": "X", "colors": {} }), "theme ID"),
            (json!({ "id": "x", "name": " ", "colors": {} }), "name"),
            (
                json!({ "id": "x", "name": "X", "base": "nope", "colors": {} }),
                "base",
            ),
            (
                json!({ "id": "x", "name": "X", "colors": { "background": "#f0f0f0" } }),
                "dark",
            ),
            (
                json!({ "id": "x", "name": "X", "colors": { "muted": "#303030" } }),
                "muted on background",
            ),
            (
                json!({ "id": "x", "name": "X", "colors": { "action": "#ffffff" } }),
                "text on action",
            ),
            (
                json!({ "id": "x", "name": "X", "colors": { "section": "#ffffff" } }),
                "text on section",
            ),
            (
                json!({ "id": "x", "name": "X", "colors": { "surface": "#ffffff" } }),
                "text on surface",
            ),
            (
                json!({ "id": "x", "name": "X", "colors": { "accent": "#000000" } }),
                "accent on background",
            ),
            (
                json!({ "id": "x", "name": "X", "colors": { "accentFill": "#ffffff" } }),
                "text on accentFill",
            ),
            (
                json!({ "id": "x", "name": "X", "colors": { "surface": "#707070" } }),
                "muted on surface",
            ),
            (
                json!({ "id": "x", "name": "X", "colors": { "onAccent": "#ffffff" } }),
                "onAccent on accent",
            ),
            (json!({ "id": "x", "name": "A\nB", "colors": {} }), "name"),
            (
                json!({ "id": "x", "name": format!("{}X", " ".repeat(30)), "colors": {} }),
                "name",
            ),
            (
                json!({ "id": "x", "name": "X", "colors": { "cable": "#000000" } }),
                "cable on canvas",
            ),
            (
                json!({ "id": "x", "name": "X", "colors": { "section": "#606060", "muted": "#bbbbbb" } }),
                "danger on section",
            ),
        ] {
            let message = spec(value).unwrap().resolve().unwrap_err().to_string();
            assert!(message.contains(error), "{message}");
        }
        assert!(spec(json!({ "id": "x", "name": "X", "colors": { "glow": "#ffffff" } })).is_err());
        assert!(spec(json!({ "id": "x", "name": "X", "colors": { "text": "white" } })).is_err());
    }

    #[test]
    fn files_need_version_one() {
        let file = br##"{"version":1,"id":"m","name":"M","colors":{"accent":"#ff8800"}}"##;
        assert_eq!(ThemeSpec::from_file(file).unwrap().id, "m");
        let file = br##"{"version":2,"id":"m","name":"M","colors":{}}"##;
        assert_eq!(ThemeSpec::from_file(file), Err(ThemeError::Version(2)));
    }

    #[test]
    fn ids_round_trip_as_text() {
        for text in [
            "stock",
            "file:midnight",
            "ext:judah/themes:judah.dusk-pack/dusk",
        ] {
            assert_eq!(ThemeId::from(text.to_owned()).to_string(), text);
        }
        assert_eq!(
            ThemeId::from("ext:judah/themes:judah.dusk-pack/dusk".to_owned()),
            ThemeId::Extension {
                extension: "judah/themes:judah.dusk-pack".into(),
                theme: "dusk".into()
            }
        );
    }
}
