//! Remote (owned, Tetrad; docs/remote.md): the Manage page of extensions: their state, switches,
//! settings, updates and installs. A command takes the path of a touch on Settings → Tetrad →
//! Extensions ([`Handoff::Extensions`]), so the pages of the unit stay in step. (O) The page
//! installs only from the repositories that the unit already trusts: adding, trusting and
//! removing repositories stays on the unit.

use serde::Deserialize;
use serde_json::{Value, json};
use tetrad_remote::{ExtensionsOp, Reply};
use tetrad_ui::{
    ExtensionCommand, ExtensionSettingInput, ExtensionSettingValue, ExtensionState, ExtensionsView,
    InstalledExtension,
};

use super::platform::Platform;
use super::ui::Handoff;
use super::{Device, DeviceError, Subsystem};

/// One command of the page: `{"op": "disable", "key": "owner/repo:id"}`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Command {
    /// Reads the catalog of each enabled repository again.
    Refresh,
    Install {
        source: String,
        id: String,
    },
    Update {
        key: String,
    },
    Enable {
        key: String,
    },
    Disable {
        key: String,
    },
    Retry {
        key: String,
    },
    /// The page asks first. The settings and the stored data stay, as on the unit.
    Remove {
        key: String,
    },
    Run {
        key: String,
        command: String,
    },
    /// `value`: the option index of a choice, `true` or `false` for a toggle, an integer for a
    /// number.
    Setting {
        key: String,
        setting: String,
        value: Value,
    },
}

impl<P: Platform> Device<P> {
    /// The list, also as the answer to a command: the page shows the new state at once.
    pub(super) fn remote_extensions(&mut self, op: ExtensionsOp) -> Reply {
        if let ExtensionsOp::Command { json } = op
            && let Err(reason) = self.remote_extension_command(&json)
        {
            return Reply::Refused(reason);
        }
        Reply::Json(listed(&self.bridges.extensions.view(&self.state.community)))
    }

    fn remote_extension_command(&mut self, json: &[u8]) -> Result<(), String> {
        if self.platform.extensions().is_none() {
            return Err("Extensions do not run in this mode".into());
        }
        let command = serde_json::from_slice::<Command>(json)
            .map_err(|error| format!("Not a command: {error}"))?;
        let command = checked(
            &self.bridges.extensions.view(&self.state.community),
            command,
        )?;
        self.notice(
            Subsystem::Remote,
            format!("Remote: extensions: {command:?}"),
        );
        use tetrad_dsp_host::session::Clock;
        let now = self.clock.now();
        let effects =
            (self.handoff(Handoff::Extensions(command), now)).map_err(|error| error.to_string())?;
        self.apply(Ok::<_, DeviceError>(effects));
        Ok(())
    }
}

/// The touch command for `command`, when the view offers it. The bridge ignores what it does
/// not know; the page gets the reason instead.
fn checked(view: &ExtensionsView, command: Command) -> Result<ExtensionCommand, String> {
    let installed = |key: &str| {
        (view.installed.iter())
            .find(|extension| extension.key == key)
            .ok_or_else(|| format!("{key} is not installed"))
    };
    Ok(match command {
        Command::Refresh => ExtensionCommand::Refresh,
        Command::Install { source, id } => {
            let offered = (view.available.iter()).any(|e| e.source == source && e.id == id);
            if !offered {
                return Err(format!("{source} does not offer {id}; refresh the list"));
            }
            ExtensionCommand::Install { source, id }
        }
        Command::Update { key } => {
            if installed(&key)?.update.is_none() {
                return Err(format!("{key} has no update"));
            }
            ExtensionCommand::Update { key }
        }
        Command::Enable { key } => {
            installed(&key)?;
            ExtensionCommand::SetEnabled { key, enabled: true }
        }
        Command::Disable { key } => {
            installed(&key)?;
            ExtensionCommand::SetEnabled {
                key,
                enabled: false,
            }
        }
        Command::Retry { key } => {
            if !matches!(installed(&key)?.state, ExtensionState::Faulted(_)) {
                return Err(format!("{key} has no fault"));
            }
            ExtensionCommand::Retry { key }
        }
        Command::Remove { key } => {
            installed(&key)?;
            ExtensionCommand::Remove { key }
        }
        Command::Run { key, command } => {
            if !(installed(&key)?.commands.iter()).any(|button| button.id == command) {
                return Err(format!("{key} has no command {command} now"));
            }
            ExtensionCommand::Run { key, command }
        }
        Command::Setting {
            key,
            setting,
            value,
        } => {
            let extension = installed(&key)?;
            let spec = (extension.settings.iter())
                .find(|spec| spec.key == setting)
                .ok_or_else(|| format!("{key} has no setting {setting}"))?;
            let input = match &spec.value {
                ExtensionSettingValue::Choice { options, .. } => (value.as_u64())
                    .and_then(|index| usize::try_from(index).ok())
                    .filter(|&index| index < options.len())
                    .map(ExtensionSettingInput::Choice),
                ExtensionSettingValue::Toggle(_) => {
                    value.as_bool().map(ExtensionSettingInput::Toggle)
                }
                ExtensionSettingValue::Number { min, max, .. } => (value.as_i64())
                    .filter(|number| (min..=max).contains(&number))
                    .map(ExtensionSettingInput::Number),
            };
            let value = input.ok_or_else(|| format!("{value} is not a value of {}", spec.label))?;
            ExtensionCommand::SetSetting {
                key,
                setting,
                value,
            }
        }
    })
}

/// The view as the page reads it (docs/remote.md). The sources are there to read only.
fn listed(view: &ExtensionsView) -> Value {
    let available: Vec<_> = (view.available.iter())
        .map(|extension| {
            json!({
                "source": extension.source,
                "id": extension.id,
                "name": extension.name,
                "version": extension.version,
                "description": extension.description,
                "duplicates": extension.duplicates,
            })
        })
        .collect();
    let sources: Vec<_> = (view.sources.iter())
        .map(|source| {
            json!({
                "repository": source.repository,
                "default": source.default,
                "enabled": source.enabled,
                "status": source.status,
            })
        })
        .collect();
    json!({
        "installed": view.installed.iter().map(installed).collect::<Vec<_>>(),
        "available": available,
        "sources": sources,
        "status": view.status,
        "busy": view.busy,
    })
}

fn installed(extension: &InstalledExtension) -> Value {
    let (state, fault) = match &extension.state {
        ExtensionState::Disabled => ("off", None),
        ExtensionState::Starting => ("starting", None),
        ExtensionState::Running => ("running", None),
        ExtensionState::Faulted(reason) => ("faulted", Some(reason)),
    };
    let settings: Vec<_> = (extension.settings.iter())
        .map(|setting| {
            let (key, label) = (&setting.key, &setting.label);
            match &setting.value {
                ExtensionSettingValue::Choice { options, selected } => json!({
                    "key": key, "label": label, "kind": "choice",
                    "options": options, "value": selected,
                }),
                ExtensionSettingValue::Toggle(on) => json!({
                    "key": key, "label": label, "kind": "toggle", "value": on,
                }),
                ExtensionSettingValue::Number { value, min, max } => json!({
                    "key": key, "label": label, "kind": "number",
                    "value": value, "min": min, "max": max,
                }),
            }
        })
        .collect();
    let commands: Vec<_> = (extension.commands.iter())
        .map(|button| json!({ "id": button.id, "label": button.label }))
        .collect();
    let blocks: Vec<_> = (extension.blocks.iter())
        .map(|block| json!({ "name": block.name, "detail": block.detail, "ready": block.ready }))
        .collect();
    json!({
        "key": extension.key,
        "name": extension.name,
        "version": extension.version,
        "description": extension.description,
        "source": extension.source,
        "enabled": extension.enabled,
        "state": state,
        "fault": fault,
        "update": extension.update,
        "duplicates": extension.duplicates,
        "commands": commands,
        "settings": settings,
        "status": extension.status_lines,
        "blocks": blocks,
        "log": extension.log,
        "themes": extension.themes,
        "mode": (extension.footswitch_mode.as_ref())
            .map(|mode| json!({ "name": mode.name, "detail": mode.detail })),
    })
}
