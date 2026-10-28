//! Remote (owned, Tetrad; docs/remote.md): the Themes page: the themes of the unit, and theme files made in the browser.
//!
//! Every op answers with the list, so the page shows the new state at once. A saved file is
//! checked with the rules of a file on the card (`ThemeSpec::resolve`). The list reads the
//! themes folder again, as Settings → Appearance does when it opens, so the unit sees a saved
//! or deleted file at once.

use std::path::Path;

use serde_json::{Value, json};
use tetrad_remote::{Reply, ThemesOp};
use tetrad_theme::{ThemeId, ThemeSpec, builtin};

use super::appearance::{MAX_FILE, MAX_THEME_FILES, THEMES_DIR};
use super::platform::{Platform, ServiceBackends};
use super::{Device, Effect, Subsystem};

/// The palette roles as the editor shows them: theme file name, label, where it shows, and
/// whether `resolve` derives it when the theme leaves it out (docs/themes.md).
const FIELDS: [(&str, &str, &str, bool); 18] = [
    (
        "background",
        "Background",
        "Page background, dropdown fields, black buttons",
        false,
    ),
    (
        "canvas",
        "Canvas",
        "Grid, tuner and tempo background; block tiles; menus",
        false,
    ),
    ("panel", "Panel", "Cards, lists and parameter panels", false),
    (
        "surface",
        "Surface",
        "Selected menu rows, top-bar buttons, unselected tabs",
        false,
    ),
    (
        "rowSelected",
        "Row selected",
        "Selected preset row; knob track",
        false,
    ),
    ("section", "Section", "Section bars and grey buttons", false),
    (
        "divider",
        "Divider",
        "Lines between rows on cards; follows background",
        true,
    ),
    ("scrollbar", "Scroll bar", "Scroll bar", false),
    ("text", "Text", "Main text", false),
    ("muted", "Muted", "Secondary text: titles, units", false),
    ("faint", "Faint", "Unselected and disabled text", false),
    ("cable", "Cable", "Grid cables", false),
    (
        "accent",
        "Accent",
        "Selected items, switches, knob arcs, meters",
        false,
    ),
    (
        "accentFill",
        "Accent fill",
        "Fill under a selected tab; mixed from accent",
        true,
    ),
    (
        "onAccent",
        "On accent",
        "Text on accent and warning fills; black or white",
        true,
    ),
    (
        "action",
        "Action",
        "Main action buttons (Save, START CAPTURE)",
        false,
    ),
    ("danger", "Danger", "Record, mute and errors", false),
    (
        "warning",
        "Warning",
        "Scene badges and \"New\" badges",
        false,
    ),
];

/// A `.json` file of [`THEMES_DIR`] and its theme, `None` when it does not parse.
struct ThemeFile {
    name: String,
    spec: Option<ThemeSpec>,
}

impl ThemeFile {
    /// Whether this file is the theme `file:<id>`. A file that does not parse goes by its whole
    /// file name, as in `Appearance::set_files`.
    fn holds(&self, id: &str) -> bool {
        match &self.spec {
            Some(spec) => spec.id == id,
            None => self.name == id,
        }
    }
}

impl<P: Platform> Device<P> {
    pub(super) fn remote_themes(&mut self, op: ThemesOp) -> Reply {
        let done = match op {
            ThemesOp::List => Ok(()),
            ThemesOp::Save { json } => self.save_theme(&json),
            ThemesOp::Select { id } => self.select_theme(ThemeId::from(id)),
            ThemesOp::Delete { id } => self.delete_theme(ThemeId::from(id)),
        };
        match done {
            Ok(()) => self.themes_list(),
            Err(reason) => Reply::Refused(reason),
        }
    }

    /// Every theme of Settings → Appearance with its colours, and for theme files and extension
    /// themes the base and colours they set, so the editor can start from them.
    fn themes_list(&mut self) -> Reply {
        self.read_theme_files();
        let files = self.theme_files();
        let extensions = self.bridges.extensions.themes().unwrap_or_default();
        let view = self.bridges.appearance.view();
        let active = (view.themes.iter()).find(|row| row.selected);
        let themes: Vec<Value> = (view.themes.iter())
            .map(|row| {
                let (source, spec) = match &row.id {
                    ThemeId::Builtin(_) => ("builtin", None),
                    ThemeId::File(id) => (
                        "file",
                        (files.iter())
                            .find(|file| file.holds(id))
                            .and_then(|file| file.spec.as_ref()),
                    ),
                    ThemeId::Extension { extension, theme } => (
                        "extension",
                        (extensions.iter())
                            .filter(|pack| &pack.extension == extension)
                            .flat_map(|pack| &pack.themes)
                            .find(|spec| &spec.id == theme),
                    ),
                };
                json!({
                    "id": row.id.to_string(),
                    "name": row.name,
                    "source": source,
                    "origin": row.source,
                    "error": row.palette.as_ref().err(),
                    "palette": row.palette.as_ref().ok(),
                    "base": spec.and_then(|spec| spec.base.as_ref()),
                    // `null`: a theme file that does not parse, so the editor cannot start from it.
                    "colors": match (&row.id, spec) {
                        (ThemeId::Builtin(_), _) => Some(json!({})),
                        (_, spec) => spec.map(|spec| json!(spec.colors)),
                    },
                })
            })
            .collect();
        let fields: Vec<Value> = (FIELDS.iter())
            .map(|&(key, label, help, derived)| {
                json!({ "key": key, "label": label, "help": help, "derived": derived })
            })
            .collect();
        Reply::Json(json!({
            "active": active.map(|row| row.id.to_string()),
            "fields": fields,
            "themes": themes,
        }))
    }

    /// Checks a theme file from the page and writes it as the file that holds its id, or as
    /// `<id>.json`.
    fn save_theme(&mut self, json: &[u8]) -> Result<(), String> {
        use tetrad_services::exec::Files;
        if json.len() as u64 > MAX_FILE {
            return Err(format!("the file is larger than {} KiB", MAX_FILE / 1024));
        }
        let spec = ThemeSpec::from_file(json).map_err(|e| e.to_string())?;
        spec.resolve().map_err(|e| e.to_string())?;
        if builtin(&spec.id).is_some() {
            return Err(format!(
                "theme ID {:?} is a built-in theme; choose another ID",
                spec.id
            ));
        }
        let files = self.theme_files();
        // (O) Saving an ID again replaces its file: the page asks first.
        let name = match files.iter().find(|file| file.holds(&spec.id)) {
            Some(file) => file.name.clone(),
            None => {
                let name = format!("{}.json", spec.id);
                if files.iter().any(|file| file.name == name) {
                    return Err(format!(
                        "{name} holds another theme or cannot be read; delete it first"
                    ));
                }
                if files.len() >= MAX_THEME_FILES {
                    return Err(format!(
                        "the unit reads at most {MAX_THEME_FILES} theme files; delete one first"
                    ));
                }
                name
            }
        };
        let dir = Path::new(THEMES_DIR);
        let files = self.platform.services().files();
        (files.create_dirs(dir))
            .and_then(|()| files.write_atomic(&dir.join(&name), &spec.to_file()))
            .map_err(|error| format!("{name} not saved: {error}"))?;
        self.notice(
            Subsystem::Remote,
            format!("Remote: theme file {name} saved"),
        );
        Ok(())
    }

    /// The touch UI's own path, so the screen repaints in the theme and `appearance.json` keeps
    /// it.
    fn select_theme(&mut self, id: ThemeId) -> Result<(), String> {
        let refused = (self.set_theme(id).into_iter()).find_map(|effect| match effect {
            Effect::Notice(_, reason) => Some(reason),
            _ => None,
        });
        refused.map_or(Ok(()), Err)
    }

    /// Removes every file of `file:<id>`. (O) The active theme falls back to Stock, chosen as on
    /// the unit, rather than staying stored for a file that is gone.
    fn delete_theme(&mut self, id: ThemeId) -> Result<(), String> {
        use tetrad_services::exec::Files;
        let ThemeId::File(file_id) = &id else {
            return Err("only theme files can be deleted".into());
        };
        let names: Vec<String> = (self.theme_files().into_iter())
            .filter(|file| file.holds(file_id))
            .map(|file| file.name)
            .collect();
        if names.is_empty() {
            return Err(format!("theme {id} is not on the unit"));
        }
        let active =
            (self.bridges.appearance.view().themes.iter()).any(|r| r.selected && r.id == id);
        let dir = Path::new(THEMES_DIR);
        let files = self.platform.services().files();
        for name in &names {
            (files.remove(&dir.join(name)))
                .map_err(|error| format!("{name} not deleted: {error}"))?;
        }
        let names = names.join(", ");
        self.notice(
            Subsystem::Remote,
            format!("Remote: theme file {names} deleted"),
        );
        match active {
            true => self.select_theme(ThemeId::stock()),
            false => Ok(()),
        }
    }

    /// The `.json` files of [`THEMES_DIR`] in name order, parsed. A file that cannot be read
    /// counts as one that does not parse.
    fn theme_files(&mut self) -> Vec<ThemeFile> {
        use tetrad_services::exec::Files;
        let files = self.platform.services().files();
        let dir = Path::new(THEMES_DIR);
        let names = (files.list_dir(dir).ok().flatten().into_iter().flatten())
            .filter(|entry| !entry.is_dir && entry.name.ends_with(".json"))
            .map(|entry| entry.name);
        names
            .map(|name| {
                let bytes = files
                    .read_bounded(&dir.join(&name), MAX_FILE)
                    .ok()
                    .flatten();
                let spec = (bytes.filter(|bytes| bytes.len() as u64 <= MAX_FILE))
                    .and_then(|bytes| ThemeSpec::from_file(&bytes).ok());
                ThemeFile { name, spec }
            })
            .collect()
    }
}
