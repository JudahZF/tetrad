//! Updater: check, download, verify (`gzip -t` only, as stock), stage and install CorOS packages.

pub mod cloud;
pub mod install;
pub mod manager;
pub mod state;

/// Stock updater paths (ZenUI `_INIT_359`, VA 0x00091824, slots 0x0154b070..0x0154b0a0).
pub mod paths {
    pub const TEMP_DIR: &str = "/media/p4/UpdaterTemp";
    pub const SCRIPT_DIR: &str = "/opt/neuraldsp";
    pub const STATUS_FILE: &str = "/etc/updater_status";
    pub const CHANGELOG: &str = "/media/p4/changelog";
    pub const CHANGELOG_BOOT: &str = "/media/p4/changelog_boot";
    pub const DOWNLOADED_FLAG: &str = "/media/p4/UpdaterTemp/updater_new_version_downloaded";
    pub const AVAILABLE_FLAG: &str = "/media/p4/UpdaterTemp/updater_new_version_available";
    pub const DOWNLOAD_FNAME: &str = "/media/p4/UpdaterTemp/update-download-fname";
    pub const DOWNLOAD_FSIZE: &str = "/media/p4/UpdaterTemp/update-download-fsize";
    pub const UPDATE_BIN: &str = "/media/p4/UpdaterTemp/update.bin";
    /// Installer scratch copies, removed as stale before a new download.
    pub const UPDATE_TMP_BIN: &str = "/media/p4/UpdaterTemp/update.tmp.bin";
    pub const UPDATE_TMP_GZ: &str = "/media/p4/UpdaterTemp/update.tmp.bin.gz";
}
