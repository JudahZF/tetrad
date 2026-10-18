//! The stock settings files and the injected storage boundary.

/// One stock `QSettings` INI file on the user partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SettingsFileId {
    /// Device state: display, I/O, MIDI, tuner, library UI state.
    Zenjack,
    /// Flags meant to outlive a settings reset.
    Permanent,
    /// Cloud account. Holds credentials; never log its values.
    Account,
    /// Plugin licences.
    PluginLicenses,
}

impl SettingsFileId {
    pub const ALL: [Self; 4] = [
        Self::Zenjack,
        Self::Permanent,
        Self::Account,
        Self::PluginLicenses,
    ];

    /// Debounce of the stock settings class: every `setValue` restarts a
    /// single-shot timer; on timeout it runs `QSettings::sync` and a global
    /// `sync()` (`0x0073b60c`). 100 ms (`0x0073b92c`), 250 ms for
    /// ZenjackConfig (`0x00650568`). Account identity changes and logout
    /// sync at once (`0x002ff47c`, `0x002ff6e0`).
    pub const fn save_delay(self) -> std::time::Duration {
        std::time::Duration::from_millis(match self {
            Self::Zenjack => 250,
            _ => 100,
        })
    }

    /// Stock absolute path.
    pub const fn path(self) -> &'static str {
        match self {
            Self::Zenjack => "/media/p4/ZenjackConfig.ini",
            Self::Permanent => "/media/p4/PermanentSettings.ini",
            Self::Account => "/media/p4/AccountConfig.ini",
            Self::PluginLicenses => "/media/p4/PluginLicenses.ini",
        }
    }
}

/// File access for settings. The app injects the real or a test backend.
pub trait SettingsStorage {
    /// `Ok(None)` when the file does not exist.
    fn read(&self, file: SettingsFileId) -> std::io::Result<Option<Vec<u8>>>;
    /// Replaces the whole file. Stock follows each `QSettings::sync` with a
    /// global `sync()`; a real backend should do the same.
    fn write(&mut self, file: SettingsFileId, bytes: &[u8]) -> std::io::Result<()>;
    /// Deletes the file; a missing file is not an error.
    fn remove(&mut self, file: SettingsFileId) -> std::io::Result<()>;
}
