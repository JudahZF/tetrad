//! The extension manifest: identity, API level, capabilities, settings, command labels, DSP
//! blocks, themes and a footswitch mode.
//!
//! A manifest is authored as `tetrad-extension.json` and travels inside every package. The
//! host trusts nothing in it until [`Manifest::validate`] passes. API 2 adds `blocks`; an API 1
//! manifest cannot use them. API 3 adds `pressed` on trigger firings (a release of a press); a
//! host before API 3 would take a release for a press, so it skips API 3 packages. API 4 adds
//! `footswitchMode`; a manifest of an older API cannot use it. API 5 adds the leaky ReLU network
//! activation; a host before API 5 refuses it, so it skips API 5 packages. API 6 adds the `remote`
//! capability: a tab on the Remote page. API 7 adds `app`: a native program that the package
//! brings and that takes the screen and the footswitches while it runs. API 8 adds the `ble`
//! capability: messages from the Tetrad app over Bluetooth, and events pushed to it.

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tetrad_theme::ThemeSpec;

/// The newest extension API level this host runs.
pub const API_VERSION: u32 = 8;
/// The oldest extension API level this host still runs.
pub const MIN_API_VERSION: u32 = 1;

pub const MAX_ID: usize = 64;
pub const MAX_NAME: usize = 48;
pub const MAX_DESCRIPTION: usize = 280;
pub const MAX_SETTINGS: usize = 16;
pub const MAX_COMMANDS: usize = 16;
pub const MAX_OPTIONS: usize = 32;
pub const MAX_BLOCKS: usize = 16;
/// Most file extensions of one block asset.
pub const MAX_ASSET_EXTENSIONS: usize = 8;
/// Longest asset file extension, the dot included.
pub const MAX_ASSET_EXTENSION: usize = 16;
/// Most themes one package may add.
pub const MAX_THEMES: usize = 8;
/// Longest setting key, setting label, option label, command ID and command label.
pub const MAX_LABEL: usize = 32;
/// Longest footswitch mode name: the Grid and Gig View headers show it.
pub const MAX_MODE_NAME: usize = 16;
/// Most pages of a footswitch mode.
pub const MAX_MODE_PAGES: u8 = 4;
/// Longest title of a Remote tab.
pub const MAX_TAB_TITLE: usize = 20;
/// Most arguments of a program.
pub const MAX_APP_ARGS: usize = 16;
/// Longest argument of a program.
pub const MAX_APP_ARG: usize = 256;
/// Stands for the folder of the program's files in an argument (`{app}/doom1.wad`).
pub const APP_DIR_PLACEHOLDER: &str = "{app}";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: Version,
    #[serde(default)]
    pub description: String,
    pub api: u32,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub settings: Vec<SettingSpec>,
    /// Labels for the command buttons. The extension registers handlers by the same IDs; a
    /// registered ID without an entry here shows its ID as the label.
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    /// DSP blocks (API 2). Skipped when empty, so an API 1 manifest stays readable by older
    /// hosts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<BlockSpec>,
    /// Colour themes for Settings → Appearance. Data only: the host reads them from the
    /// manifest and runs no code for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub themes: Vec<ThemeSpec>,
    /// A footswitch mode (API 4). Data only: Tetrad runs its switches; the extension declares
    /// its name and page count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footswitch_mode: Option<FootswitchModeSpec>,
    /// A tab on the Remote page (API 6). Needs the `remote` capability, and the package holds
    /// the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteSpec>,
    /// A native program (API 7). The package holds its files; the extension's page starts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<AppSpec>,
}

/// A native program of the package, such as a game. While it runs, Tetrad does not draw the
/// screen or act on the footswitches, the knobs and TEMPO; the sound goes on. It runs as Tetrad
/// does, with full access to the unit, in the extension's asset folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppSpec {
    /// The button on the extension's page, such as "Play DOOM"; 1..=[`MAX_LABEL`] characters.
    pub label: String,
    /// The file to start: one of the package's program files.
    pub exec: String,
    /// At most [`MAX_APP_ARGS`] arguments of 1..=[`MAX_APP_ARG`] characters.
    /// [`APP_DIR_PLACEHOLDER`] in an argument becomes the folder of the program files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// The tab of an extension on the Remote page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteSpec {
    /// 1..=[`MAX_TAB_TITLE`] characters: the name on the tab.
    pub title: String,
}

/// A footswitch mode next to Preset, Scene and Stomp: `pages` pages of ten switches (A–D,
/// Down, E–H, Tap) whose actions the user chooses on the unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FootswitchModeSpec {
    /// 1..=[`MAX_MODE_NAME`] characters, such as "Custom".
    pub name: String,
    /// 1..=[`MAX_MODE_PAGES`].
    pub pages: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Capability {
    MidiReceive,
    MidiSend,
    ControlsRead,
    ControlsEdit,
    Storage,
    /// Declare triggers and fire them into Mappings.
    Triggers,
    /// Provide generated blocks: build qcdsp IR from an asset file at run time (API 2).
    Blocks,
    /// Answer messages from a computer over the USB cable (`tetrad-usb ext`, docs/usb.md).
    Usb,
    /// A tab on the Remote page (API 6): a page that the package holds, and a handler for its
    /// messages (docs/remote.md, "Extension tabs").
    Remote,
    /// Messages from the Tetrad app over Bluetooth, and events pushed to it (API 8,
    /// docs/bluetooth.md).
    Ble,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingSpec {
    pub key: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: SettingKind,
}

/// What the device can render. The config value of a choice is its option's `value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SettingKind {
    /// `default` indexes `options`.
    Choice {
        options: Vec<ChoiceOption>,
        default: usize,
    },
    Toggle {
        default: bool,
    },
    Number {
        min: i64,
        max: i64,
        default: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceOption {
    /// A JSON scalar: string, number or boolean.
    pub value: Value,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    pub id: String,
    pub label: String,
}

/// A DSP block. Tetrad validates its qcdsp IR and compiles it to SHARC code; the IR's name,
/// blob name and category are replaced from this entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlockSpec {
    /// 1..=[`MAX_LABEL`] characters of `[a-z0-9-]`, unique in the manifest.
    pub id: String,
    /// Display name, 1..=[`MAX_LABEL`] characters.
    pub name: String,
    pub category: qcdsp::Category,
    /// `false`: the package holds the IR. `true`: the extension builds the IR at run time
    /// from an asset file (`ctx.blocks.provide`); needs the `blocks` capability and a module.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub generated: bool,
    /// The asset file a generated block is built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetSpec>,
    /// The stock model ID written in presets for stock CorOS; `None` uses Tetrad's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<u32>,
}

/// The files a user can choose for a generated block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetSpec {
    /// What the file is, such as "NAM model"; 1..=[`MAX_LABEL`] characters.
    pub label: String,
    /// 1..=[`MAX_ASSET_EXTENSIONS`] lowercase extensions with the dot, such as `.nam`.
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error(
        "extension ID {0:?} must be at least two dot-separated segments of [a-z0-9-], at most {MAX_ID} characters"
    )]
    Id(String),
    #[error("name must be 1..={MAX_NAME} characters")]
    Name,
    #[error("description must be at most {MAX_DESCRIPTION} characters")]
    Description,
    #[error("API {0} is not supported; this host runs API {MIN_API_VERSION} to {API_VERSION}")]
    Api(u32),
    #[error("{1} needs API {0}")]
    NeedsApi(u32, &'static str),
    #[error("capability {0:?} is listed twice")]
    DuplicateCapability(Capability),
    #[error("at most {MAX_SETTINGS} settings, {MAX_COMMANDS} commands and {MAX_BLOCKS} blocks")]
    TooMany,
    #[error(
        "setting key {0:?} must be 1..={MAX_LABEL} characters of [A-Za-z0-9_], starting with a letter"
    )]
    SettingKey(String),
    #[error("{0:?} is declared twice")]
    Duplicate(String),
    #[error("{0:?}: labels must be 1..={MAX_LABEL} characters")]
    Label(String),
    #[error("setting {0:?}: {1}")]
    Setting(String, &'static str),
    #[error("command ID {0:?} must be 1..={MAX_LABEL} characters of [a-z0-9-]")]
    CommandId(String),
    #[error("block {0:?}: {1}")]
    Block(String, &'static str),
    #[error("at most {MAX_THEMES} themes")]
    TooManyThemes,
    #[error("theme {0:?}: {1}")]
    Theme(String, String),
    #[error(
        "footswitch mode: the name must be 1..={MAX_MODE_NAME} characters and pages 1..={MAX_MODE_PAGES}"
    )]
    FootswitchMode,
    #[error(
        "remote: a tab needs both the remote capability and a title of 1..={MAX_TAB_TITLE} characters"
    )]
    Remote,
    #[error("app: {0}")]
    App(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("config must be a JSON object")]
    NotObject,
    #[error("setting {0:?} has a wrong type or an out-of-range value")]
    Invalid(String),
}

impl Manifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        validate_id(&self.id)?;
        if !label_ok(&self.name, MAX_NAME) {
            return Err(ManifestError::Name);
        }
        if self.description.chars().count() > MAX_DESCRIPTION {
            return Err(ManifestError::Description);
        }
        if !(MIN_API_VERSION..=API_VERSION).contains(&self.api) {
            return Err(ManifestError::Api(self.api));
        }
        if self.api < 2 && (!self.blocks.is_empty() || self.has(Capability::Blocks)) {
            return Err(ManifestError::NeedsApi(2, "blocks"));
        }
        if self.api < 4 && self.footswitch_mode.is_some() {
            return Err(ManifestError::NeedsApi(4, "footswitchMode"));
        }
        if let Some(mode) = &self.footswitch_mode
            && (!label_ok(&mode.name, MAX_MODE_NAME) || !(1..=MAX_MODE_PAGES).contains(&mode.pages))
        {
            return Err(ManifestError::FootswitchMode);
        }
        if self.api < 6 && (self.remote.is_some() || self.has(Capability::Remote)) {
            return Err(ManifestError::NeedsApi(6, "remote"));
        }
        if self.api < 8 && self.has(Capability::Ble) {
            return Err(ManifestError::NeedsApi(8, "ble"));
        }
        match &self.remote {
            Some(tab) if self.has(Capability::Remote) && label_ok(&tab.title, MAX_TAB_TITLE) => {}
            None if !self.has(Capability::Remote) => {}
            _ => return Err(ManifestError::Remote),
        }
        if self.api < 7 && self.app.is_some() {
            return Err(ManifestError::NeedsApi(7, "app"));
        }
        if let Some(app) = &self.app {
            app.validate()?;
        }
        for (i, capability) in self.capabilities.iter().enumerate() {
            if self.capabilities[..i].contains(capability) {
                return Err(ManifestError::DuplicateCapability(*capability));
            }
        }
        if self.settings.len() > MAX_SETTINGS
            || self.commands.len() > MAX_COMMANDS
            || self.blocks.len() > MAX_BLOCKS
        {
            return Err(ManifestError::TooMany);
        }
        for (i, setting) in self.settings.iter().enumerate() {
            setting.validate()?;
            if self.settings[..i].iter().any(|s| s.key == setting.key) {
                return Err(ManifestError::Duplicate(setting.key.clone()));
            }
        }
        for (i, command) in self.commands.iter().enumerate() {
            if !short_id_ok(&command.id) {
                return Err(ManifestError::CommandId(command.id.clone()));
            }
            if !label_ok(&command.label, MAX_LABEL) {
                return Err(ManifestError::Label(command.id.clone()));
            }
            if self.commands[..i].iter().any(|c| c.id == command.id) {
                return Err(ManifestError::Duplicate(command.id.clone()));
            }
        }
        for (i, block) in self.blocks.iter().enumerate() {
            block.validate(self.has(Capability::Blocks))?;
            if self.blocks[..i].iter().any(|b| b.id == block.id) {
                return Err(ManifestError::Duplicate(block.id.clone()));
            }
        }
        if self.themes.len() > MAX_THEMES {
            return Err(ManifestError::TooManyThemes);
        }
        for (i, theme) in self.themes.iter().enumerate() {
            if let Err(error) = theme.resolve() {
                return Err(ManifestError::Theme(theme.id.clone(), error.to_string()));
            }
            if self.themes[..i].iter().any(|t| t.id == theme.id) {
                return Err(ManifestError::Duplicate(theme.id.clone()));
            }
        }
        Ok(())
    }

    pub fn block(&self, id: &str) -> Option<&BlockSpec> {
        self.blocks.iter().find(|b| b.id == id)
    }

    /// A data-only manifest adds themes, static DSP blocks or a footswitch mode and nothing
    /// else: no capabilities, settings, commands, generated blocks or program. Its package may
    /// leave out the module; the host runs nothing for a package without one (a package with a
    /// module still runs it).
    pub fn data_only(&self) -> bool {
        !(self.themes.is_empty() && self.blocks.is_empty() && self.footswitch_mode.is_none())
            && self.app.is_none()
            && !self.needs_module()
    }

    /// A program with nothing that needs a module: its package may leave the module out.
    pub fn app_only(&self) -> bool {
        self.app.is_some() && !self.needs_module()
    }

    /// Capabilities, settings, commands and generated blocks work through the module.
    fn needs_module(&self) -> bool {
        !self.capabilities.is_empty()
            || !self.settings.is_empty()
            || !self.commands.is_empty()
            || self.blocks.iter().any(|b| b.generated)
    }

    pub fn has(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// Every setting at its default, as a JSON object.
    pub fn default_config(&self) -> Value {
        Value::Object(
            self.settings
                .iter()
                .map(|s| (s.key.clone(), s.default_value()))
                .collect(),
        )
    }

    /// Checks a stored or user-edited config against the settings, fills missing keys with
    /// their defaults and drops keys of settings the manifest no longer has (an update can
    /// remove a setting).
    pub fn validate_config(&self, config: &Value) -> Result<Value, ConfigError> {
        let object = config.as_object().ok_or(ConfigError::NotObject)?;
        let mut out = Map::new();
        for setting in &self.settings {
            let value = match object.get(&setting.key) {
                None => setting.default_value(),
                Some(value) if setting.accepts(value) => value.clone(),
                Some(_) => return Err(ConfigError::Invalid(setting.key.clone())),
            };
            out.insert(setting.key.clone(), value);
        }
        Ok(Value::Object(out))
    }
}

impl SettingSpec {
    fn validate(&self) -> Result<(), ManifestError> {
        let mut key = self.key.bytes();
        let key_ok = self.key.len() <= MAX_LABEL
            && key.next().is_some_and(|b| b.is_ascii_alphabetic())
            && key.all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !key_ok {
            return Err(ManifestError::SettingKey(self.key.clone()));
        }
        if !label_ok(&self.label, MAX_LABEL) {
            return Err(ManifestError::Label(self.key.clone()));
        }
        let fail = |reason| Err(ManifestError::Setting(self.key.clone(), reason));
        match &self.kind {
            SettingKind::Choice { options, default } => {
                if options.is_empty() || options.len() > MAX_OPTIONS {
                    return fail("a choice needs 1..=32 options");
                }
                if *default >= options.len() {
                    return fail("default is not an option index");
                }
                for (i, option) in options.iter().enumerate() {
                    if !matches!(
                        option.value,
                        Value::String(_) | Value::Number(_) | Value::Bool(_)
                    ) {
                        return fail("option values must be strings, numbers or booleans");
                    }
                    if !label_ok(&option.label, MAX_LABEL) {
                        return fail("option labels must be 1..=32 characters");
                    }
                    if options[..i].iter().any(|o| o.value == option.value) {
                        return fail("option values must be unique");
                    }
                }
            }
            SettingKind::Toggle { .. } => {}
            SettingKind::Number { min, max, default } => {
                if min > max || !(min..=max).contains(&default) {
                    return fail("needs min <= default <= max");
                }
            }
        }
        Ok(())
    }

    pub fn default_value(&self) -> Value {
        match &self.kind {
            SettingKind::Choice { options, default } => options[*default].value.clone(),
            SettingKind::Toggle { default } => Value::Bool(*default),
            SettingKind::Number { default, .. } => Value::from(*default),
        }
    }

    fn accepts(&self, value: &Value) -> bool {
        match &self.kind {
            SettingKind::Choice { options, .. } => options.iter().any(|o| &o.value == value),
            SettingKind::Toggle { .. } => value.is_boolean(),
            SettingKind::Number { min, max, .. } => {
                value.as_i64().is_some_and(|v| (min..=max).contains(&&v))
            }
        }
    }
}

impl BlockSpec {
    fn validate(&self, capability: bool) -> Result<(), ManifestError> {
        let fail = |reason| Err(ManifestError::Block(self.id.clone(), reason));
        if !short_id_ok(&self.id) {
            return fail("the ID must be 1..=32 characters of [a-z0-9-]");
        }
        if !label_ok(&self.name, MAX_LABEL) {
            return fail("the name must be 1..=32 characters");
        }
        if self.generated && !capability {
            return fail("a generated block needs the blocks capability");
        }
        let Some(asset) = &self.asset else {
            return Ok(());
        };
        if !self.generated {
            return fail("only a generated block has an asset");
        }
        if !label_ok(&asset.label, MAX_LABEL) {
            return fail("the asset label must be 1..=32 characters");
        }
        if !(1..=MAX_ASSET_EXTENSIONS).contains(&asset.extensions.len()) {
            return fail("an asset needs 1..=8 file extensions");
        }
        for (i, extension) in asset.extensions.iter().enumerate() {
            let ok = extension.len() <= MAX_ASSET_EXTENSION
                && extension.strip_prefix('.').is_some_and(|rest| {
                    !rest.is_empty()
                        && rest
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                });
            if !ok {
                return fail("asset extensions are a dot and 1..=15 characters of [a-z0-9]");
            }
            if asset.extensions[..i].contains(extension) {
                return fail("asset extensions must be unique");
            }
        }
        Ok(())
    }
}

impl AppSpec {
    fn validate(&self) -> Result<(), ManifestError> {
        if !label_ok(&self.label, MAX_LABEL) {
            return Err(ManifestError::App("the label must be 1..=32 characters"));
        }
        if !file_name_ok(&self.exec) {
            return Err(ManifestError::App("exec must be a program file name"));
        }
        let arg_ok =
            |arg: &String| (1..=MAX_APP_ARG).contains(&arg.chars().count()) && !arg.contains('\0');
        if self.args.len() > MAX_APP_ARGS || !self.args.iter().all(arg_ok) {
            return Err(ManifestError::App(
                "at most 16 arguments of 1..=256 characters, without NUL",
            ));
        }
        Ok(())
    }
}

/// A program file name: 1..=64 characters of `[A-Za-z0-9._-]`, not starting with `.`.
pub fn file_name_ok(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Command and block IDs: 1..=[`MAX_LABEL`] characters of `[a-z0-9-]`.
pub(crate) fn short_id_ok(id: &str) -> bool {
    (1..=MAX_LABEL).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Namespaced ID: two or more `.`-separated segments of `[a-z0-9-]`, at most [`MAX_ID`].
pub fn validate_id(id: &str) -> Result<(), ManifestError> {
    let segment_ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    if id.len() <= MAX_ID && id.split('.').count() >= 2 && id.split('.').all(segment_ok) {
        Ok(())
    } else {
        Err(ManifestError::Id(id.to_owned()))
    }
}

fn label_ok(text: &str, max: usize) -> bool {
    let len = text.chars().count();
    (1..=max).contains(&len) && !text.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> Manifest {
        serde_json::from_value(json!({
            "id": "tetrad.test", "name": "Test", "version": "1.2.3", "api": 1,
            "capabilities": ["midiReceive", "storage"],
            "settings": [
                {"key": "channel", "label": "Channel", "kind": "choice", "default": 1,
                 "options": [{"value": 1, "label": "1"}, {"value": 2, "label": "2"}]},
                {"key": "armed", "label": "Armed", "kind": "toggle", "default": true},
                {"key": "depth", "label": "Depth", "kind": "number", "min": 0, "max": 10, "default": 5}
            ],
            "commands": [{"id": "learn", "label": "Learn mapping"}]
        }))
        .unwrap()
    }

    #[test]
    fn ids_are_namespaced() {
        for good in ["tetrad.midi-mapper", "a.b.c", "x1.y-2"] {
            assert!(validate_id(good).is_ok(), "{good}");
        }
        let long = format!("a.{}", "b".repeat(MAX_ID));
        for bad in [
            "single",
            "a..b",
            "A.b",
            "a.b_c",
            ".a.b",
            "a.b.",
            long.as_str(),
        ] {
            assert!(validate_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn config_fills_defaults_and_rejects_bad_values() {
        let m = manifest();
        m.validate().unwrap();
        assert_eq!(
            m.default_config(),
            json!({"channel": 2, "armed": true, "depth": 5})
        );
        assert_eq!(
            m.validate_config(&json!({"depth": 7})).unwrap(),
            json!({"channel": 2, "armed": true, "depth": 7})
        );
        for bad in [
            json!({"channel": 3}),
            json!({"armed": 1}),
            json!({"depth": 11}),
            json!({"depth": 1.5}),
        ] {
            assert!(matches!(
                m.validate_config(&bad),
                Err(ConfigError::Invalid(_))
            ));
        }
        assert_eq!(
            m.validate_config(&json!({"other": 1, "depth": 7})).unwrap(),
            json!({"channel": 2, "armed": true, "depth": 7}),
            "a setting removed by an update is dropped"
        );
    }

    #[test]
    fn invalid_settings_are_refused() {
        let mut m = manifest();
        m.settings[2].kind = SettingKind::Number {
            min: 0,
            max: 10,
            default: 11,
        };
        assert!(matches!(m.validate(), Err(ManifestError::Setting(..))));
        let mut m = manifest();
        m.settings[1].key = "channel".into();
        assert!(matches!(m.validate(), Err(ManifestError::Duplicate(_))));
        let mut m = manifest();
        m.api = API_VERSION + 1;
        assert_eq!(m.validate(), Err(ManifestError::Api(API_VERSION + 1)));
    }

    #[test]
    fn a_footswitch_mode_needs_api_4_and_is_data_only() {
        let mode: Manifest = serde_json::from_value(json!({
            "id": "tetrad.custom-footswitch", "name": "Custom Footswitch", "version": "0.1.0",
            "api": 4, "footswitchMode": {"name": "Custom", "pages": 4}
        }))
        .unwrap();
        assert_eq!(mode.validate(), Ok(()));
        assert!(mode.data_only());
        let mut old = mode.clone();
        old.api = 3;
        assert_eq!(
            old.validate(),
            Err(ManifestError::NeedsApi(4, "footswitchMode"))
        );
        for (name, pages) in [
            ("", 4),
            ("Seventeen chars!!", 4),
            ("Custom", 0),
            ("Custom", 5),
        ] {
            let mut bad = mode.clone();
            bad.footswitch_mode = Some(FootswitchModeSpec {
                name: name.into(),
                pages,
            });
            assert_eq!(
                bad.validate(),
                Err(ManifestError::FootswitchMode),
                "{name:?} {pages}"
            );
        }
    }

    fn block(extra: serde_json::Value) -> BlockSpec {
        let mut spec = json!({"id": "amp", "name": "Amp", "category": "guitar_amplifier"});
        spec.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(spec).unwrap()
    }

    #[test]
    fn block_rules() {
        let mut m = manifest();
        m.api = 2;
        m.capabilities.push(Capability::Blocks);
        let asset = json!({"label": "NAM model", "extensions": [".nam"]});
        m.blocks = vec![
            block(json!({})),
            block(json!({"id": "nam", "generated": true, "asset": asset, "placeholder": 1001})),
        ];
        m.validate().unwrap();

        let mut api1 = m.clone();
        api1.api = 1;
        assert_eq!(api1.validate(), Err(ManifestError::NeedsApi(2, "blocks")));
        let mut duplicate = m.clone();
        duplicate.blocks[1].id = "amp".into();
        assert!(matches!(
            duplicate.validate(),
            Err(ManifestError::Duplicate(_))
        ));
        let mut too_many = m.clone();
        too_many.blocks = (0..=MAX_BLOCKS)
            .map(|i| block(json!({"id": format!("b{i}")})))
            .collect();
        assert_eq!(too_many.validate(), Err(ManifestError::TooMany));

        let refused = [
            (json!({"id": "Amp"}), true),
            (json!({"name": ""}), true),
            (json!({"generated": true}), false),
            (
                json!({"asset": {"label": "File", "extensions": [".nam"]}}),
                true,
            ),
            (
                json!({"generated": true, "asset": {"label": "File", "extensions": []}}),
                true,
            ),
            (
                json!({"generated": true, "asset": {"label": "File", "extensions": ["nam"]}}),
                true,
            ),
            (
                json!({"generated": true, "asset": {"label": "File", "extensions": [".NAM"]}}),
                true,
            ),
        ];
        for (extra, capability) in refused {
            let spec = block(extra.clone());
            assert!(
                matches!(spec.validate(capability), Err(ManifestError::Block(..))),
                "{extra}"
            );
        }
    }

    #[test]
    fn a_remote_tab_needs_the_capability_the_title_and_api_6() {
        let base = |extra: serde_json::Value| -> Result<(), ManifestError> {
            let mut value = serde_json::json!({
                "id": "tetrad.tabbed", "name": "Tabbed", "version": "1.0.0", "api": 6,
            });
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value::<Manifest>(value)
                .unwrap()
                .validate()
        };
        let tab = serde_json::json!({"capabilities": ["remote"], "remote": {"title": "Models"}});
        assert_eq!(base(tab), Ok(()));
        // One without the other, or a title that is empty or too long.
        assert_eq!(
            base(serde_json::json!({"capabilities": ["remote"]})),
            Err(ManifestError::Remote)
        );
        assert_eq!(
            base(serde_json::json!({"remote": {"title": "Models"}})),
            Err(ManifestError::Remote)
        );
        for title in ["", "A title of more than twenty characters"] {
            let tab = serde_json::json!({"capabilities": ["remote"], "remote": {"title": title}});
            assert_eq!(base(tab), Err(ManifestError::Remote), "{title:?}");
        }
        // API 5 hosts know nothing of tabs, so an API 5 manifest cannot have one.
        let mut old = serde_json::json!({
            "id": "tetrad.tabbed", "name": "Tabbed", "version": "1.0.0", "api": 5,
            "capabilities": ["remote"], "remote": {"title": "Models"}
        });
        assert_eq!(
            serde_json::from_value::<Manifest>(old.take())
                .unwrap()
                .validate(),
            Err(ManifestError::NeedsApi(6, "remote"))
        );
    }

    #[test]
    fn a_program_needs_api_7_a_label_and_a_file_name() {
        let app = |api: u32, app: serde_json::Value| {
            serde_json::from_value::<Manifest>(json!({
                "id": "tetrad.doom", "name": "DOOM", "version": "0.1.0", "api": api, "app": app
            }))
            .unwrap()
        };
        let doom =
            json!({"label": "Play DOOM", "exec": "qcdoom", "args": ["-iwad", "{app}/doom1.wad"]});
        let m = app(7, doom.clone());
        assert_eq!(m.validate(), Ok(()));
        assert!(
            m.app_only() && !m.data_only(),
            "a program is code, not data"
        );
        assert_eq!(
            app(6, doom).validate(),
            Err(ManifestError::NeedsApi(7, "app"))
        );
        for bad in [
            json!({"label": "", "exec": "qcdoom"}),
            json!({"label": "Play", "exec": "../qcdoom"}),
            json!({"label": "Play", "exec": ".hidden"}),
            json!({"label": "Play", "exec": "qcdoom", "args": [""]}),
            json!({"label": "Play", "exec": "qcdoom", "args": ["a\u{0}b"]}),
            json!({"label": "Play", "exec": "qcdoom", "args": vec!["x"; MAX_APP_ARGS + 1]}),
        ] {
            assert!(
                matches!(app(7, bad.clone()).validate(), Err(ManifestError::App(_))),
                "{bad}"
            );
        }
    }
}
