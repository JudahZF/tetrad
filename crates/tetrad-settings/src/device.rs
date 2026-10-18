//! The typed settings snapshot over the four stock files: load, typed
//! updates with change events, debounced flush, boot migrations and resets.

use thiserror::Error;
use zeroize::Zeroizing;

use crate::account::{AccountIdentity, AccountTokens, PairedVersion, PluginLicenses};
use crate::capture::CaptureSettings;
use crate::controls::{AtmaVolume, LooperSettings, ModeSettings};
use crate::general::{Display, GeneralSettings, GlobalBypassRows, MasterVolumeAssignment};
use crate::io::{ExpressionCalibration, GlobalEq, IoSettings};
use crate::legacy::{LegacyModelDefaults, find_legacy_model_defaults};
use crate::library::{BackupDownloadTimes, LastPreset, LibraryTables, SetlistCreationTimes};
use crate::midi::MidiSettings;
use crate::permanent::{CloudDomain, NewModels, PermanentFlags};
use crate::tuner::{TempoDefaults, TempoSettings, TunerSettings};
use crate::typed::{KeyReader, KeyWriter};
use crate::{LoadIssue, LoadIssueKind, SettingsFile, SettingsFileId, SettingsStorage};

/// Hardware type (`VersionDeviceType`: Qc 0, Atma 1). Selects the stock
/// defaults that differ (`0x00499a60` checks).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Hardware {
    #[default]
    QuadCortex,
    Atma,
}

#[derive(Debug, Error)]
#[error("settings file {} ({op}): {source}", file.path())]
pub struct StorageError {
    pub file: SettingsFileId,
    pub op: &'static str,
    #[source]
    pub source: std::io::Error,
}

/// One table defines the snapshot, the event enum, load and store.
macro_rules! settings_groups {
    ($($(#[$meta:meta])* $field:ident: $ty:ty => $event:ident in $file:ident),+ $(,)?) => {
        /// Typed snapshot of every modelled stock setting.
        #[derive(Clone, Debug, PartialEq)]
        pub struct Settings {
            $($(#[$meta])* pub $field: $ty,)+
        }

        /// A group changed; carries its new value.
        #[derive(Clone, Debug, PartialEq)]
        pub enum SettingsEvent {
            $($event($ty),)+
        }

        impl SettingsEvent {
            pub fn file(&self) -> SettingsFileId {
                match self {
                    $(Self::$event(_) => SettingsFileId::$file,)+
                }
            }
        }

        impl Settings {
            fn load(files: &Files, hardware: Hardware, issues: &mut Vec<LoadIssue>) -> Self {
                Self {
                    $($field: <$ty>::load(&mut KeyReader {
                        file: files.get(SettingsFileId::$file),
                        id: SettingsFileId::$file,
                        hardware,
                        issues,
                    }),)+
                }
            }

            /// Events for groups that differ from `old`, in table order.
            fn diff(&self, old: &Self) -> Vec<SettingsEvent> {
                let mut events = Vec::new();
                $(if self.$field != old.$field {
                    events.push(SettingsEvent::$event(self.$field.clone()));
                })+
                events
            }

            /// Writes the fields that differ from `old` into their files.
            fn store_diff(&self, old: &Self, files: &mut Files) {
                $(if self.$field != old.$field {
                    self.$field.store(&old.$field, &mut KeyWriter { file: files.get_mut(SettingsFileId::$file) });
                })+
            }

            /// Reloads the groups of one file.
            fn reload(&mut self, id: SettingsFileId, files: &Files, hardware: Hardware) -> Vec<LoadIssue> {
                let mut issues = Vec::new();
                $(if SettingsFileId::$file == id {
                    self.$field = <$ty>::load(&mut KeyReader { file: files.get(id), id, hardware, issues: &mut issues });
                })+
                issues
            }
        }
    };
}

settings_groups! {
    display: Display => Display in Zenjack,
    general: GeneralSettings => General in Zenjack,
    master_volume: MasterVolumeAssignment => MasterVolume in Zenjack,
    global_bypass: GlobalBypassRows => GlobalBypass in Zenjack,
    midi: MidiSettings => Midi in Zenjack,
    io: IoSettings => Io in Zenjack,
    expression: ExpressionCalibration => Expression in Zenjack,
    global_eq: GlobalEq => GlobalEq in Zenjack,
    tuner: TunerSettings => Tuner in Zenjack,
    tempo: TempoSettings => Tempo in Zenjack,
    mode: ModeSettings => Mode in Zenjack,
    looper: LooperSettings => Looper in Zenjack,
    atma_volume: AtmaVolume => AtmaVolume in Zenjack,
    capture: CaptureSettings => Capture in Zenjack,
    library: LibraryTables => Library in Zenjack,
    last_preset: LastPreset => LastPreset in Zenjack,
    backup_download_times: BackupDownloadTimes => BackupDownloadTimes in Zenjack,
    setlist_creation_times: SetlistCreationTimes => SetlistCreationTimes in Zenjack,
    permanent: PermanentFlags => Permanent in Permanent,
    cloud_domain: CloudDomain => CloudDomain in Permanent,
    new_models: NewModels => NewModels in Permanent,
    account: AccountIdentity => Account in Account,
    tokens: AccountTokens => Tokens in Account,
    paired_version: PairedVersion => PairedVersion in Account,
    licenses: PluginLicenses => Licenses in PluginLicenses,
}

impl Settings {
    /// Stock defaults: what stock reads from empty files.
    pub fn defaults(hardware: Hardware) -> Self {
        Self::load(&Files::default(), hardware, &mut Vec::new())
    }
}

/// The four files, indexed by [`SettingsFileId`].
#[derive(Clone, Debug, Default)]
struct Files([SettingsFile; 4]);

impl Files {
    fn get(&self, id: SettingsFileId) -> &SettingsFile {
        &self.0[id as usize]
    }

    fn get_mut(&mut self, id: SettingsFileId) -> &mut SettingsFile {
        &mut self.0[id as usize]
    }
}

/// Which reset (`SuspendConnection` reasons FACTORY_RESET / SETTINGS_RESET;
/// device menu "FACTORY RESET" / "RESET SETTINGS", `0x004de79c` /
/// `0x004decdc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResetKind {
    Settings,
    Factory,
}

/// Reset work outside the settings files. The integration must run these, in the order of
/// [`ResetKind::effects`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResetEffect {
    /// Factory: close `/media/p4/zenui.db` (`0x0069daac`), delete it (`0x006a551c`) and
    /// create it again (`0x006a5fd0` → `0x006a576c`: schema patches, user roots), then
    /// wipe the user files (`0x00770334`: `user_preset_folders`, favorites and recents,
    /// `Presets` and `Plugins`; `Presets/My Presets` and the plugin folders come back empty).
    ResetDatabase,
    /// Factory: clear the licence manager in memory and in the DSP (`0x001f0c50`: licensed
    /// set and flags cleared, empty `0x51`). [`DeviceSettings::reset`] deletes the file.
    ClearLicences,
    /// Delete `/media/p4/recentSearches` (`0x007a5da4`), both kinds.
    RemoveRecentSearches,
    /// Clear `ZenUI.conf` (native `QSettings`, `0x0073b898` then sync), both.
    ClearNativeConf,
    /// Factory: cloud unpair (`0x00308250`), then `user/signout` and the logout
    /// (`0x002ff6e0`).
    CloudUnpair,
    /// Remove all saved Wi-Fi networks (`0x00895264`), both kinds. Factory: after
    /// [`Self::CloudUnpair`]; the networks must stay until the unpair requests are done.
    RemoveWifiNetworks,
    /// `0x004de3ec`: remove `/opt/neuraldsp/productiontestmode`,
    /// `/etc/updater_status` and the updater "new version" flags; rewrite
    /// `/opt/neuraldsp/presetsDirPath`. Both kinds.
    ResetSystemFlags,
    /// Factory (`0x004de3ec(1)`): also remove `/media/p4/coredump` and
    /// `/opt/neuraldsp/allow_sshd`.
    RemoveDebugFiles,
    /// Factory: load preset 0 of the Factory Library (`0x005b37f0`); the load also clears
    /// the undo history (`0x00647ac0`).
    LoadFactoryPreset,
    /// Factory: write `0` to `/opt/neuraldsp/boot_count.txt` (`0x00459014`).
    ResetBootCount,
}

impl ResetKind {
    /// The steps in stock order (factory `0x004de79c`).
    pub const fn effects(self) -> &'static [ResetEffect] {
        use ResetEffect::*;
        match self {
            Self::Settings => &[
                RemoveWifiNetworks,
                RemoveRecentSearches,
                ClearNativeConf,
                ResetSystemFlags,
            ],
            Self::Factory => &[
                ResetDatabase,
                ClearLicences,
                RemoveRecentSearches,
                ClearNativeConf,
                CloudUnpair,
                RemoveWifiNetworks,
                ResetSystemFlags,
                RemoveDebugFiles,
                LoadFactoryPreset,
                ResetBootCount,
            ],
        }
    }
}

/// Owns the stock files and the typed snapshot. Keys the model does not
/// know stay in the files untouched.
#[derive(Clone, Debug)]
pub struct DeviceSettings {
    files: Files,
    settings: Settings,
    hardware: Hardware,
    /// PluginLicenses.ini must be deleted at the next flush.
    delete_licenses: bool,
}

impl DeviceSettings {
    /// Reads all files. A missing file is empty (stock defaults apply);
    /// values that do not load are reported and fall back like stock.
    pub fn load(
        storage: &impl SettingsStorage,
        hardware: Hardware,
    ) -> Result<(Self, Vec<LoadIssue>), StorageError> {
        let mut files = Files::default();
        let mut issues = Vec::new();
        for id in SettingsFileId::ALL {
            let bytes = storage.read(id).map_err(|source| StorageError {
                file: id,
                op: "read",
                source,
            })?;
            if let Some(bytes) = bytes {
                // The read buffer of `PluginLicenses.ini` and `AccountConfig.ini` holds licence
                // data and tokens: zeroed on drop (review r11 R6).
                let bytes = Zeroizing::new(bytes);
                // Qt keeps going on FormatError; the keys it could read are used.
                let (file, format_issues) = SettingsFile::parse(&bytes);
                issues.extend(format_issues.into_iter().map(|issue| LoadIssue {
                    file: id,
                    key: None,
                    kind: LoadIssueKind::Format(issue),
                }));
                *files.get_mut(id) = file;
            }
        }
        let settings = Settings::load(&files, hardware, &mut issues);
        Ok((
            Self {
                files,
                settings,
                hardware,
                delete_licenses: false,
            },
            issues,
        ))
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn hardware(&self) -> Hardware {
        self.hardware
    }

    /// Raw file access for keys the typed model does not cover. Changes made
    /// here to modelled keys are not seen until the next load.
    pub fn file(&self, id: SettingsFileId) -> &SettingsFile {
        self.files.get(id)
    }

    /// Applies a typed change, writes the changed groups into their files
    /// and returns one event per changed group.
    pub fn update(&mut self, change: impl FnOnce(&mut Settings)) -> Vec<SettingsEvent> {
        let old = self.settings.clone();
        change(&mut self.settings);
        self.settings.store_diff(&old, &mut self.files);
        self.settings.diff(&old)
    }

    /// Stock boot steps that change settings (main `0x0007dd68`): legacy
    /// tempo layout, then missing tempo params filled with `tempo_defaults`
    /// (must run before any tempo change), `NewModels` seeding and expiry
    /// after more than [`NewModels::EXPIRY_BOOTS`] boots, and token wipe when
    /// no user is stored.
    pub fn migrate_on_boot(
        &mut self,
        boot_count: u32,
        tempo_defaults: &TempoDefaults,
    ) -> Vec<SettingsEvent> {
        let mut events = Vec::new();
        if TempoSettings::migrate_legacy(self.files.get_mut(SettingsFileId::Zenjack)) {
            let old = self.settings.tempo.clone();
            self.settings
                .reload(SettingsFileId::Zenjack, &self.files, self.hardware);
            if self.settings.tempo != old {
                events.push(SettingsEvent::Tempo(self.settings.tempo.clone()));
            }
        }
        events.extend(self.update(|s| {
            s.tempo.fill_missing(tempo_defaults);
            s.new_models.seed();
            if boot_count > NewModels::EXPIRY_BOOTS {
                s.new_models.expire();
            }
            if s.account.is_logged_out() {
                s.tokens = AccountTokens::default();
            }
        }));
        events
    }

    /// Legacy per-model default groups (see [`crate::legacy`]), read only.
    /// Precondition (stock `0x006a4140`): the user database version is not
    /// 300. The caller checks that before it calls this.
    pub fn legacy_model_defaults(&self) -> Vec<LegacyModelDefaults> {
        find_legacy_model_defaults(self.files.get(SettingsFileId::Zenjack))
    }

    /// Removes imported legacy groups from ZenjackConfig.ini (written at the
    /// next flush). Call it only after the database import succeeded: a
    /// failed import keeps the groups for the next boot.
    pub fn remove_legacy_model_defaults(&mut self, imported: &[LegacyModelDefaults]) {
        let file = self.files.get_mut(SettingsFileId::Zenjack);
        for legacy in imported {
            file.remove(&legacy.group);
        }
    }

    /// Stock reset of the settings files. Returns the change events and the
    /// effects outside the settings files ([`ResetKind::effects`]).
    ///
    /// Both kinds clear ZenjackConfig.ini (`0x00650d28`) and keep
    /// PermanentSettings.ini. The I/O reset that follows (`0x003615a0`)
    /// pairs both output pairs. Factory reset also logs out (identity and
    /// tokens written as `""`, `0x002ff6e0`), resets the last preset to
    /// the default setlist (the cleared state) and deletes
    /// PluginLicenses.ini (`0x001f0c50`). Logout keeps `last_paired_version`.
    /// The tempo defaults are written after the clear, as stock does.
    pub fn reset(&mut self, kind: ResetKind, tempo_defaults: &TempoDefaults) -> Vec<SettingsEvent> {
        let old = self.settings.clone();
        self.files.get_mut(SettingsFileId::Zenjack).clear();
        self.settings
            .reload(SettingsFileId::Zenjack, &self.files, self.hardware);
        let cleared = self.settings.clone();
        self.settings.tempo.fill_missing(tempo_defaults);
        self.settings.io.out12_paired = true;
        self.settings.io.out34_paired = true;
        if kind == ResetKind::Factory {
            self.settings.account = AccountIdentity::default();
            self.settings.tokens = AccountTokens::default();
            self.settings.licenses = PluginLicenses::default();
        }
        self.settings.store_diff(&cleared, &mut self.files);
        if kind == ResetKind::Factory {
            *self.files.get_mut(SettingsFileId::PluginLicenses) = SettingsFile::new();
            self.delete_licenses = true;
        }
        self.settings.diff(&old)
    }

    /// Files with unsaved changes.
    pub fn dirty_files(&self) -> impl Iterator<Item = SettingsFileId> + '_ {
        SettingsFileId::ALL.into_iter().filter(|&id| {
            self.files.get(id).is_dirty()
                || (id == SettingsFileId::PluginLicenses && self.delete_licenses)
        })
    }

    /// Writes every dirty file. Call it [`SettingsFileId::save_delay`] after
    /// the last update of a file, and before power-off.
    pub fn flush(&mut self, storage: &mut impl SettingsStorage) -> Result<(), StorageError> {
        if self.delete_licenses {
            let id = SettingsFileId::PluginLicenses;
            storage.remove(id).map_err(|source| StorageError {
                file: id,
                op: "remove",
                source,
            })?;
            self.delete_licenses = false;
        }
        for id in SettingsFileId::ALL {
            let file = self.files.get_mut(id);
            if file.is_dirty() {
                // The buffer can hold tokens and licence data.
                storage
                    .write(id, &Zeroizing::new(file.to_bytes()))
                    .map_err(|source| StorageError {
                        file: id,
                        op: "write",
                        source,
                    })?;
                file.save();
            }
        }
        Ok(())
    }
}
