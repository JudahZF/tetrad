//! Extensions view models and commands. The application fills [`ExtensionsView`]; the
//! Extensions settings pages show it and answer with [`ExtensionCommand`]s. The UI never
//! decides trust, installation or extension state; it shows what the host reports.

/// Everything the Extensions pages show.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtensionsView {
    pub installed: Vec<InstalledExtension>,
    /// Catalog entries from every enabled source that are not installed, source-labelled.
    pub available: Vec<AvailableExtension>,
    pub sources: Vec<ExtensionSource>,
    /// A repository waiting for the user to trust its signing key.
    pub pending_trust: Option<PendingTrust>,
    /// The running job or the last result, such as "Installing MIDI Mapper…".
    pub status: Option<String>,
    /// A repository job (refresh, install, update, trust) is running.
    pub busy: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstalledExtension {
    /// Stable host key of the installation, `owner/repo:extension.id`.
    pub key: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// `owner/repo`; the default repository is labelled by the host, not the catalog.
    pub source: String,
    pub enabled: bool,
    pub state: ExtensionState,
    /// A newer version in the source's catalog.
    pub update: Option<String>,
    /// Command buttons the extension registered.
    pub commands: Vec<ExtensionButton>,
    pub settings: Vec<ExtensionSetting>,
    /// Bounded text lines the extension set with `scope.ui.setStatus`.
    pub status_lines: Vec<String>,
    /// The newest log lines, oldest first.
    pub log: Vec<String>,
    /// Names of the themes it adds to Settings → Appearance.
    pub themes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionState {
    Disabled,
    Starting,
    Running,
    /// Stopped after a fault; RETRY starts a new session.
    Faulted(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionButton {
    pub id: String,
    pub label: String,
}

/// One manifest-declared setting with its current value.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionSetting {
    pub key: String,
    pub label: String,
    pub value: ExtensionSettingValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionSettingValue {
    /// Pick one of `options` (labels); `selected` indexes them.
    Choice {
        options: Vec<String>,
        selected: usize,
    },
    Toggle(bool),
    Number {
        value: i64,
        min: i64,
        max: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableExtension {
    /// `owner/repo`.
    pub source: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionSource {
    /// `owner/repo`.
    pub repository: String,
    /// The bundled default repository (host-labelled).
    pub default: bool,
    pub enabled: bool,
    /// Key fingerprint shown to the user, such as `3f2a 9c01 …`.
    pub fingerprint: String,
    /// "Up to date", "Not refreshed", "Refresh failed: …".
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTrust {
    pub repository: String,
    pub fingerprint: String,
}

/// A user request from the Extensions pages. The host validates every one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionCommand {
    /// Fetch every enabled source's catalog.
    Refresh,
    Install {
        source: String,
        id: String,
    },
    Update {
        key: String,
    },
    SetEnabled {
        key: String,
        enabled: bool,
    },
    /// Remove the installation; its settings and storage stay.
    Remove {
        key: String,
    },
    /// Start a faulted extension again.
    Retry {
        key: String,
    },
    Run {
        key: String,
        command: String,
    },
    SetSetting {
        key: String,
        setting: String,
        value: ExtensionSettingInput,
    },
    /// `owner/repo` or its GitHub HTTPS URL; the host fetches its key and asks for trust.
    AddSource {
        repository: String,
    },
    /// The answer to [`ExtensionsView::pending_trust`].
    TrustSource {
        repository: String,
        trust: bool,
    },
    SetSourceEnabled {
        repository: String,
        enabled: bool,
    },
    RemoveSource {
        repository: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionSettingInput {
    Choice(usize),
    Toggle(bool),
    Number(i64),
}
