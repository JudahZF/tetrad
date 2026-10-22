//! Appearance bridge (owned, Tetrad): the colour theme of the touch UI, behind Settings →
//! Appearance (`tetrad_ui::AppearanceView`).
//!
//! Themes come from three places: the built-in themes, theme files in [`THEMES_DIR`], and
//! the manifests of enabled extensions. The choice is stored in [`FILE`] with the palette it
//! had, so a boot shows the theme from the first frame, before the extension store is read.
//! A chosen theme that is gone (its file deleted, its extension disabled) shows Stock; the
//! choice stays stored, so the theme comes back with its source.

use serde::{Deserialize, Serialize};
use tetrad_theme::{BUILTINS, Palette, ThemeId, ThemeSpec};
use tetrad_ui::{AppearanceView, ThemeRow};

pub const FILE: &str = "/media/p4/tetrad/appearance.json";
pub const THEMES_DIR: &str = "/media/p4/tetrad/themes";
/// Largest theme file and appearance file. Read them with `read_bounded(.., MAX_FILE)`: it
/// returns one byte more for a larger file, and the bridge refuses that.
pub const MAX_FILE: u64 = 64 * 1024;
/// Theme files read at most, in name order.
pub const MAX_THEME_FILES: usize = 32;
const FORMAT: u32 = 1;

/// [`FILE`]: the chosen theme and its colours when it was chosen.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Stored {
    version: u32,
    theme: ThemeId,
    palette: Palette,
}

/// Themes one enabled extension adds.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionThemes {
    /// The install key, `owner/repo:extension.id`: two repositories may ship one extension ID.
    pub extension: String,
    pub name: String,
    pub themes: Vec<ThemeSpec>,
}

/// One theme to pick from.
#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    id: ThemeId,
    name: String,
    source: String,
    palette: Result<Palette, String>,
}

#[derive(Debug, Default)]
pub struct Appearance {
    chosen: ThemeId,
    /// The chosen theme's colours from [`FILE`], shown while extension themes are unknown.
    stored: Option<Palette>,
    files: Vec<Candidate>,
    /// `None` until the extension store is read.
    extensions: Option<Vec<Candidate>>,
    /// Set by every change; the view is built again only then.
    pub(in crate::device) view_stale: bool,
}

/// `bytes` unless they are over [`MAX_FILE`]: a file cut at the limit must not parse.
fn bounded(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    match bytes.len() as u64 > MAX_FILE {
        true => Err(format!("the file is larger than {} KiB", MAX_FILE / 1024)),
        false => Ok(bytes),
    }
}

impl Appearance {
    /// The stored choice. A bad file is reported and Stock shows.
    pub fn load(&mut self, bytes: Option<&[u8]>) -> Option<String> {
        self.view_stale = true;
        if bytes.is_some_and(|bytes| bytes.len() as u64 > MAX_FILE) {
            return Some(format!("{FILE}: larger than {} KiB", MAX_FILE / 1024));
        }
        let stored: Stored = match bytes.map(serde_json::from_slice) {
            None => return None,
            Some(Ok(stored)) => stored,
            Some(Err(error)) => return Some(format!("{FILE}: {error}")),
        };
        if stored.version != FORMAT {
            return Some(format!(
                "{FILE}: version {} is not supported",
                stored.version
            ));
        }
        self.chosen = stored.theme;
        self.stored = tetrad_theme::check(&stored.palette)
            .is_ok()
            .then_some(stored.palette);
        None
    }

    /// The theme files of [`THEMES_DIR`] by file name: their bytes, or why they could not be
    /// read. Files with the same theme id after the first are refused.
    pub fn set_files(&mut self, files: Vec<(String, Result<Vec<u8>, String>)>) {
        self.view_stale = true;
        self.files.clear();
        for (file, bytes) in files {
            let spec = bytes
                .and_then(bounded)
                .and_then(|bytes| ThemeSpec::from_file(&bytes).map_err(|e| e.to_string()));
            let candidate = match spec {
                Ok(spec) => {
                    let duplicate =
                        (self.files.iter()).any(|c| c.id == ThemeId::File(spec.id.clone()));
                    Candidate {
                        id: ThemeId::File(spec.id.clone()),
                        palette: if duplicate {
                            Err(format!("theme ID {:?} is used by an earlier file", spec.id))
                        } else {
                            spec.resolve().map_err(|e| e.to_string())
                        },
                        name: spec.name,
                        source: format!("Theme file {file}"),
                    }
                }
                Err(reason) => Candidate {
                    // The whole file name: theme IDs have no `.`, so it never takes a real ID.
                    id: ThemeId::File(file.clone()),
                    name: file.clone(),
                    source: "Theme file".into(),
                    palette: Err(reason),
                },
            };
            self.files.push(candidate);
        }
    }

    /// Themes of the enabled extensions. Manifests were checked at install, so their themes
    /// resolve; a failure still shows as a row that cannot be picked. Returns a notice when
    /// the chosen theme went away with its extension.
    pub fn set_extensions(&mut self, extensions: Vec<ExtensionThemes>) -> Option<String> {
        self.view_stale = true;
        let had = self.resolves(&self.chosen);
        self.extensions = Some(
            (extensions.into_iter())
                .flat_map(|extension| {
                    (extension.themes.into_iter()).map(move |spec| Candidate {
                        id: ThemeId::Extension {
                            extension: extension.extension.clone(),
                            theme: spec.id.clone(),
                        },
                        palette: spec.resolve().map_err(|e| e.to_string()),
                        name: spec.name,
                        source: extension.name.clone(),
                    })
                })
                .collect(),
        );
        let gone =
            matches!(self.chosen, ThemeId::Extension { .. }) && had && !self.resolves(&self.chosen);
        gone.then(|| "The chosen theme's extension is gone; Stock shows until it is back".into())
    }

    /// Choose `id`. Returns the new contents of [`FILE`], or why it cannot be chosen.
    pub fn select(&mut self, id: ThemeId) -> Result<Vec<u8>, String> {
        let palette = self
            .candidates()
            .find(|c| c.id == id)
            .ok_or_else(|| format!("theme {id} is not installed"))?
            .palette
            .clone()?;
        let bytes = serde_json::to_vec_pretty(&Stored {
            version: FORMAT,
            theme: id.clone(),
            palette,
        })
        .map_err(|e| e.to_string())?;
        self.chosen = id;
        self.stored = Some(palette);
        self.view_stale = true;
        Ok(bytes)
    }

    /// The colours the UI shows now.
    pub fn palette(&self) -> Palette {
        match self.candidates().find(|c| c.id == self.chosen) {
            Some(Candidate { palette: Ok(p), .. }) => p,
            // An extension theme before the store is read: its stored colours.
            None if self.pending(&self.chosen) => self.stored.unwrap_or(Palette::STOCK),
            _ => Palette::STOCK,
        }
    }

    pub fn view(&self) -> AppearanceView {
        let shown = if self.resolves(&self.chosen) || self.pending(&self.chosen) {
            self.chosen.clone()
        } else {
            ThemeId::stock()
        };
        AppearanceView {
            palette: self.palette(),
            themes: (self.candidates())
                .map(|c| ThemeRow {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    source: c.source.clone(),
                    palette: c.palette.clone(),
                    selected: c.id == shown,
                })
                .collect(),
        }
    }

    fn candidates(&self) -> impl Iterator<Item = Candidate> + '_ {
        let builtins = BUILTINS.iter().map(|theme| Candidate {
            id: ThemeId::Builtin(theme.id.into()),
            name: theme.name.into(),
            source: "Built-in".into(),
            palette: Ok(theme.palette),
        });
        builtins
            .chain(self.files.iter().cloned())
            .chain(self.extensions.iter().flatten().cloned())
    }

    fn resolves(&self, id: &ThemeId) -> bool {
        self.candidates().any(|c| &c.id == id && c.palette.is_ok())
    }

    fn pending(&self, id: &ThemeId) -> bool {
        matches!(id, ThemeId::Extension { .. }) && self.extensions.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slate() -> Palette {
        tetrad_theme::builtin("slate").unwrap().palette
    }

    fn dusk() -> ExtensionThemes {
        pack("judah/themes:judah.dusk-pack", "#ff8fab")
    }

    fn pack(key: &str, accent: &str) -> ExtensionThemes {
        let spec = serde_json::json!({
            "id": "dusk", "name": "Dusk", "base": "slate", "colors": { "accent": accent }
        });
        ExtensionThemes {
            extension: key.into(),
            name: "Dusk Themes".into(),
            themes: vec![serde_json::from_value(spec).unwrap()],
        }
    }

    /// The same extension ID from two repositories gives two themes, each with its own colours.
    #[test]
    fn extension_themes_are_keyed_by_installation() {
        let mut appearance = Appearance::default();
        let other = "someone/fork:judah.dusk-pack";
        appearance.set_extensions(vec![dusk(), pack(other, "#00c2a8")]);
        let id = ThemeId::from(format!("ext:{other}/dusk"));
        appearance.select(id.clone()).unwrap();
        assert_eq!(
            appearance.palette().accent,
            tetrad_theme::Rgb::hex(0x00c2a8)
        );
        let selected: Vec<_> = (appearance.view().themes.into_iter())
            .filter(|row| row.selected)
            .map(|row| row.id)
            .collect();
        assert_eq!(selected, [id]);
        appearance.set_extensions(vec![dusk()]);
        assert_eq!(
            appearance.palette(),
            Palette::STOCK,
            "never the other pack's theme"
        );
    }

    /// A chosen extension theme shows from the first frame, falls back to Stock while its
    /// extension is gone, and comes back with it: the stored choice is kept throughout.
    #[test]
    fn extension_theme_survives_boot_and_its_extension_going_away() {
        let mut first = Appearance::default();
        first.set_extensions(vec![dusk()]);
        let id = ThemeId::from("ext:judah/themes:judah.dusk-pack/dusk".to_owned());
        let stored = first.select(id.clone()).unwrap();
        let chosen = first.palette();
        assert_eq!(chosen.panel, slate().panel);

        let mut boot = Appearance::default();
        assert_eq!(boot.load(Some(&stored)), None);
        assert_eq!(
            boot.palette(),
            chosen,
            "stored colours before the store is read"
        );

        let notice = boot.set_extensions(Vec::new());
        assert_eq!(boot.palette(), Palette::STOCK);
        assert!(
            notice.is_none(),
            "it never showed from its extension in this boot"
        );
        let view = boot.view();
        assert!(
            view.themes
                .iter()
                .any(|t| t.id == ThemeId::stock() && t.selected)
        );

        boot.set_extensions(vec![dusk()]);
        assert_eq!(boot.palette(), chosen);
        assert!(boot.set_extensions(Vec::new()).is_some());
    }

    fn oversized(json: &[u8]) -> Vec<u8> {
        let mut bytes = json.to_vec();
        bytes.resize(MAX_FILE as usize + 1, b' ');
        bytes
    }

    /// A file that does not parse holds no theme ID: a later file may use its name.
    #[test]
    fn a_broken_file_does_not_take_its_name_as_an_id() {
        let mut appearance = Appearance::default();
        let good = br##"{"version":1,"id":"midnight","name":"Midnight","colors":{}}"##;
        appearance.set_files(vec![
            ("midnight.json".into(), Err("bad JSON".into())),
            ("z.json".into(), Ok(good.to_vec())),
        ]);
        assert!(appearance.select(ThemeId::File("midnight".into())).is_ok());
    }

    #[test]
    fn oversized_appearance_file_is_refused() {
        let mut appearance = Appearance::default();
        let stored = br##"{"version":1,"theme":"slate","palette":{}}"##;
        let problem = appearance.load(Some(&oversized(stored))).unwrap();
        assert!(problem.contains("larger"), "{problem}");
    }

    #[test]
    fn theme_files_are_rows_and_bad_ones_cannot_be_chosen() {
        let mut appearance = Appearance::default();
        let good =
            br##"{"version":1,"id":"midnight","name":"Midnight","colors":{"accent":"#7aa2f7"}}"##;
        let light = br##"{"version":1,"id":"day","name":"Day","colors":{"background":"#ffffff"}}"##;
        appearance.set_files(vec![
            ("a.json".into(), Ok(good.to_vec())),
            ("b.json".into(), Ok(good.to_vec())),
            ("c.json".into(), Ok(light.to_vec())),
            ("d.json".into(), Err("permission denied".into())),
            // A valid theme padded past the limit: `read_bounded` hands over one byte more.
            ("e.json".into(), Ok(oversized(good))),
        ]);
        let rows = appearance.view().themes;
        let files: Vec<_> = rows
            .iter()
            .filter(|r| r.source.starts_with("Theme file"))
            .collect();
        assert_eq!(files.len(), 5);
        assert!(files[4].palette.as_ref().unwrap_err().contains("larger"));
        assert!(files[0].palette.is_ok());
        assert!(
            files[1]
                .palette
                .as_ref()
                .unwrap_err()
                .contains("earlier file")
        );
        assert!(files[2].palette.as_ref().unwrap_err().contains("dark"));
        assert!(appearance.select(ThemeId::File("day".into())).is_err());
        assert!(appearance.select(ThemeId::File("midnight".into())).is_ok());
        assert_eq!(
            appearance.palette().accent,
            tetrad_theme::Rgb::hex(0x7aa2f7)
        );
    }
}
