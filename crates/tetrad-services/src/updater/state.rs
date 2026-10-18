//! Updater state as Cortex Control and the UI see it, rebuilt from the stock flag files (status
//! worker VA 0x004e8824).

use std::path::Path;

use super::paths;
use crate::exec::{FileError, Files};

/// `UpdaterMessage.State`; the stock manager state numbers map one to one (table 0xa0db68).
/// 1 is published by every start function, before the work runs; 2 on `downloadStarted`; 3 on
/// `UPDATER_START`/`STEP`; 4 on `UPDATER_COMPLETE` or installer success; 5 on failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdaterState {
    Idle = 0,
    RequestingData = 1,
    DownloadingData = 2,
    Updating = 3,
    Reboot = 4,
    Failed = 5,
}

/// `UpdaterMessage.Status`. 3 is set when a check finds nothing (code 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdaterStatus {
    None = 0,
    NewVersionAvailableToDownload = 1,
    NewVersionDownloaded = 2,
    NoNewVersionAvailable = 3,
}

/// Stock rule: the downloaded flag wins over the available flag; neither is `None`.
pub fn flag_status(files: &mut impl Files) -> Result<UpdaterStatus, FileError> {
    Ok(
        if files.size(Path::new(paths::DOWNLOADED_FLAG))?.is_some() {
            UpdaterStatus::NewVersionDownloaded
        } else if files.size(Path::new(paths::AVAILABLE_FLAG))?.is_some() {
            UpdaterStatus::NewVersionAvailableToDownload
        } else {
            UpdaterStatus::None
        },
    )
}

/// Stock download bar tops out here; the rest is the install.
pub const DOWNLOAD_PERCENT_CAP: u8 = 95;

/// Download bar from `update-download-fname` (the file being written) and
/// `update-download-fsize` (expected bytes): `size / (expected / 95)`, capped at 95.
/// `None` when no download is in progress or the size file is not a positive integer (stock
/// divides by zero there).
pub fn download_percent(files: &mut impl Files) -> Result<Option<u8>, FileError> {
    let (Some(name), Some(size)) = (
        read_trimmed(files, paths::DOWNLOAD_FNAME)?,
        read_trimmed(files, paths::DOWNLOAD_FSIZE)?,
    ) else {
        return Ok(None);
    };
    let Some(expected) = size.parse::<u64>().ok().filter(|&n| n > 0) else {
        return Ok(None);
    };
    let have = files.size(Path::new(&name))?.unwrap_or(0);
    let percent = (u128::from(have) * u128::from(DOWNLOAD_PERCENT_CAP) / u128::from(expected))
        .min(u128::from(DOWNLOAD_PERCENT_CAP));
    Ok(Some(percent as u8))
}

/// A small text file without surrounding whitespace; `None` when missing or not UTF-8.
pub(crate) fn read_trimmed(
    files: &mut impl Files,
    path: &str,
) -> Result<Option<String>, FileError> {
    Ok(files
        .read(Path::new(path))?
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| text.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::MemFiles;

    #[test]
    fn flags_and_download_bar() {
        let mut files = MemFiles::default();
        assert_eq!(flag_status(&mut files).unwrap(), UpdaterStatus::None);
        files.put(paths::AVAILABLE_FLAG, "");
        files.put(paths::DOWNLOADED_FLAG, "");
        assert_eq!(
            flag_status(&mut files).unwrap(),
            UpdaterStatus::NewVersionDownloaded
        );

        assert_eq!(download_percent(&mut files).unwrap(), None);
        files.put(paths::DOWNLOAD_FNAME, format!("{}\n", paths::UPDATE_BIN));
        files.put(paths::DOWNLOAD_FSIZE, "0");
        assert_eq!(download_percent(&mut files).unwrap(), None);
        files.put(paths::DOWNLOAD_FSIZE, "200\n");
        files.put(paths::UPDATE_BIN, vec![0u8; 100]);
        assert_eq!(download_percent(&mut files).unwrap(), Some(47));
        files.put(paths::UPDATE_BIN, vec![0u8; 300]);
        assert_eq!(download_percent(&mut files).unwrap(), Some(95));
    }
}
