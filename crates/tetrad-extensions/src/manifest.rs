//! The extension manifest: identity, API level, capabilities, settings, command labels and
//! themes.
//!
//! A manifest is authored as `tetrad-extension.json` and travels inside every package. The
//! host trusts nothing in it until [`Manifest::validate`] passes.

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tetrad_theme::ThemeSpec;

/// The extension API level this host runs.
pub const API_VERSION: u32 = 1;

pub const MAX_ID: usize = 64;
pub const MAX_NAME: usize = 48;
pub const MAX_DESCRIPTION: usize = 280;
pub const MAX_SETTINGS: usize = 16;
pub const MAX_COMMANDS: usize = 16;
pub const MAX_OPTIONS: usize = 32;
/// Most themes one package may add.
pub const MAX_THEMES: usize = 8;
/// Longest setting key, setting label, option label, command ID and command label.
pub const MAX_LABEL: usize = 32;

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
    /// Colour themes for Settings → Appearance. Data only: the host reads them from the
    /// manifest and runs no code for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub themes: Vec<ThemeSpec>,
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
    /// Answer messages from a computer over the USB cable (`tetrad-usb ext`, docs/usb.md).
    Usb,
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
    #[error("API {0} is not supported; this host runs API {API_VERSION}")]
    Api(u32),
    #[error("capability {0:?} is listed twice")]
    DuplicateCapability(Capability),
    #[error("at most {MAX_SETTINGS} settings and {MAX_COMMANDS} commands")]
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
    #[error("at most {MAX_THEMES} themes")]
    TooManyThemes,
    #[error("theme {0:?}: {1}")]
    Theme(String, String),
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
        if self.api != API_VERSION {
            return Err(ManifestError::Api(self.api));
        }
        for (i, capability) in self.capabilities.iter().enumerate() {
            if self.capabilities[..i].contains(capability) {
                return Err(ManifestError::DuplicateCapability(*capability));
            }
        }
        if self.settings.len() > MAX_SETTINGS || self.commands.len() > MAX_COMMANDS {
            return Err(ManifestError::TooMany);
        }
        for (i, setting) in self.settings.iter().enumerate() {
            setting.validate()?;
            if self.settings[..i].iter().any(|s| s.key == setting.key) {
                return Err(ManifestError::Duplicate(setting.key.clone()));
            }
        }
        for (i, command) in self.commands.iter().enumerate() {
            let id_ok = !command.id.is_empty()
                && command.id.len() <= MAX_LABEL
                && command
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !id_ok {
                return Err(ManifestError::CommandId(command.id.clone()));
            }
            if !label_ok(&command.label, MAX_LABEL) {
                return Err(ManifestError::Label(command.id.clone()));
            }
            if self.commands[..i].iter().any(|c| c.id == command.id) {
                return Err(ManifestError::Duplicate(command.id.clone()));
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

    /// A data-only manifest adds themes and nothing else: no capabilities, settings or
    /// commands. Its package may leave out the module; the host runs nothing for a package
    /// without one (a package with a module still runs it).
    pub fn data_only(&self) -> bool {
        !self.themes.is_empty()
            && self.capabilities.is_empty()
            && self.settings.is_empty()
            && self.commands.is_empty()
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
        m.api = 2;
        assert_eq!(m.validate(), Err(ManifestError::Api(2)));
    }
}
