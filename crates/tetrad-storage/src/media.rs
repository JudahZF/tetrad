//! Stock CorOS user-data files under `/media/p4`.
//!
//! Stock files store absolute device paths (`/media/p4/Presets/My Presets/x.pb`).
//! [`MediaRoot`] maps those paths onto a host directory, so the same code runs
//! on the device (host root `/media/p4`) and against a copied backup.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Error, Result};

pub const DEVICE_ROOT: &str = "/media/p4";
/// Read-only factory content (`Factory Library`, `factory.db`, search index).
pub const FACTORY_ROOT: &str = "/opt/neuraldsp";
/// Slots in one stock `setlist` file (`/media/p4/Presets/<name>/setlist`).
pub const SETLIST_SLOTS: usize = 256;
/// Token for an unused setlist slot.
pub const EMPTY_SLOT: &str = "empty";

/// Fixed stock files directly under `/media/p4` (ZenUI string table VAs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFile {
    /// `0x011eeae4`.
    Database,
    /// `0x011fec88`, `0x011fec9c`, `0x011fecb4`.
    Favorites(Collection),
    /// `0x011fedd0`, `0x011fede4`, `0x011fedfc`.
    Recents(Collection),
    /// `0x011fd484`.
    RecentSearches,
    /// `user_preset_folders`, `user_nc_folders`, `user_ir_folders` (`0x00a44c50..`).
    UserFolders(Collection),
    /// `0x011f89f8`.
    SearchIndexUser,
    /// `0x011f8a14`.
    SearchRecentChangesUser,
    /// `0x00a1d448`: the preset setlist directories.
    Presets,
}

/// The three stock library collections with separate favorites/recents files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collection {
    Presets,
    Captures,
    Irs,
}

impl MediaFile {
    pub fn relative(self) -> &'static str {
        use Collection::{Captures, Irs, Presets};
        match self {
            Self::Database => "zenui.db",
            Self::Favorites(Presets) => "favorites",
            Self::Favorites(Captures) => "favorites-nc",
            Self::Favorites(Irs) => "favorites-ir",
            Self::Recents(Presets) => "recents",
            Self::Recents(Captures) => "recents-nc",
            Self::Recents(Irs) => "recents-ir",
            Self::RecentSearches => "recentSearches",
            Self::UserFolders(Presets) => "user_preset_folders",
            Self::UserFolders(Captures) => "user_nc_folders",
            Self::UserFolders(Irs) => "user_ir_folders",
            Self::SearchIndexUser => "searchIndexUser",
            Self::SearchRecentChangesUser => "searchRecentChangesUser",
            Self::Presets => "Presets",
        }
    }
}

/// A host directory that is, or holds a copy of, a device directory
/// (`/media/p4` or `/opt/neuraldsp`). The caller must control this directory.
#[derive(Debug, Clone)]
pub struct MediaRoot {
    host: PathBuf,
    device: &'static str,
}

impl MediaRoot {
    /// The user-data root, `/media/p4`.
    pub fn new(host: impl Into<PathBuf>) -> Self {
        Self {
            host: host.into(),
            device: DEVICE_ROOT,
        }
    }

    /// The factory root, `/opt/neuraldsp`.
    pub fn factory(host: impl Into<PathBuf>) -> Self {
        Self {
            host: host.into(),
            device: FACTORY_ROOT,
        }
    }

    pub fn host(&self) -> &Path {
        &self.host
    }

    pub fn device(&self) -> &'static str {
        self.device
    }

    /// True when `device` is a path below this root.
    pub fn contains(&self, device: &str) -> bool {
        device
            .strip_prefix(self.device)
            .is_some_and(|rest| rest.starts_with('/'))
    }

    pub fn file(&self, file: MediaFile) -> PathBuf {
        self.host.join(file.relative())
    }

    /// Maps a stored absolute device path to its host path. Only normal
    /// components below the device root are accepted.
    pub fn resolve(&self, device: &str) -> Result<PathBuf> {
        let relative = device
            .strip_prefix(self.device)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or_else(|| Error::DevicePath(device.to_owned()))?;
        check_relative(relative).map_err(|_| Error::DevicePath(device.to_owned()))?;
        Ok(self.host.join(relative))
    }

    /// The device path of a `/`-separated path below the device root.
    pub fn device_path(&self, relative: &str) -> Result<String> {
        check_relative(relative).map_err(|_| Error::DevicePath(relative.to_owned()))?;
        Ok(format!("{}/{relative}", self.device))
    }
}

/// Accepts a relative path of normal components that is safe in a line file.
fn check_relative(relative: &str) -> Result<()> {
    // `Path::components` drops inner `.` segments, so check the text form.
    if relative.contains(['\0', '\n', '\r', '\\'])
        || relative
            .split('/')
            .any(|segment| matches!(segment, "" | "." | ".."))
    {
        return Err(Error::Identity);
    }
    Ok(())
}

/// Checks one file or directory name for stock storage: non-empty, no path
/// separator, no `.`/`..`, no control line breaks.
pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 255
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\0', '\n', '\r'])
    {
        return Err(Error::Identity);
    }
    Ok(())
}

/// Reads a bounded file. A missing file reads as `None`; a symlink or other
/// non-regular file is refused.
pub fn read_file(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Err(Error::NotRegular);
    }
    if metadata.len() > max_bytes {
        return Err(Error::Limit);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::Limit);
    }
    Ok(Some(bytes))
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Marks the temporary sibling of [`write_atomic`]: `<name>.tetrad-tmp-<pid>-<n>`.
/// A crash can leave one behind; it is never a stock file.
pub const TEMP_MARKER: &str = ".tetrad-tmp-";
/// [`TEMP_MARKER`] as zen-rs (the earlier name of Tetrad) wrote it. Every such file is a
/// leftover now.
pub const LEGACY_TEMP_MARKER: &str = ".zen-tmp-";

/// Whether `file` is a temporary sibling that a crash left: [`TEMP_MARKER`] or
/// [`LEGACY_TEMP_MARKER`].
pub fn is_temp_file(file: &str) -> bool {
    file.contains(TEMP_MARKER) || file.contains(LEGACY_TEMP_MARKER)
}

/// Replaces `path` atomically: write a temporary sibling, fsync it, rename it
/// over `path`, then fsync the directory. A crash leaves the old or the new
/// content, never a partial file. An existing symlink at `path` is refused.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    Staged::write(path, bytes)?.commit()
}

/// The first half of [`write_atomic`]: the bytes are in a synced temporary
/// sibling of the target. [`Staged::commit`] renames it over the target; a
/// `Staged` dropped without a commit removes its temporary file. Stage every
/// file of a group before the first commit, so a write error changes nothing.
#[derive(Debug)]
pub struct Staged {
    temp: PathBuf,
    target: PathBuf,
}

impl Staged {
    /// Writes and syncs the temporary sibling of `path`. An existing symlink
    /// at `path` is refused.
    pub fn write(path: &Path, bytes: &[u8]) -> Result<Self> {
        if let Ok(metadata) = fs::symlink_metadata(path)
            && !metadata.is_file()
        {
            return Err(Error::NotRegular);
        }
        let directory = path.parent().ok_or(Error::Identity)?;
        let name = path.file_name().ok_or(Error::Identity)?;
        let mut temp_name = name.to_os_string();
        temp_name.push(format!(
            "{TEMP_MARKER}{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let temp = directory.join(temp_name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        // From here the temporary file is ours; drop removes it on an error.
        let staged = Self {
            temp,
            target: path.to_owned(),
        };
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(staged)
    }

    /// Renames the temporary file over the target, then fsyncs the directory.
    pub fn commit(self) -> Result<()> {
        fs::rename(&self.temp, &self.target)?;
        let directory = self.target.parent().ok_or(Error::Identity)?;
        File::open(directory)?.sync_all()?;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        // Best effort: the temporary sibling is ours and never the target. After a
        // commit it is gone and this fails harmlessly.
        let _ = fs::remove_file(&self.temp);
    }
}

/// Decodes a stock newline list (`favorites`, `recents`, `recentSearches`,
/// `user_*_folders`): UTF-8 lines, each ended by `\n` (ZenUI reads with
/// `readLine`, `0x0069a8cc`). A final line without `\n` is accepted. Empty
/// lines are kept, as stock keeps them: the favorites/recents files pair
/// lines by position.
pub fn decode_lines(bytes: &[u8], max_entries: usize) -> Result<Vec<String>> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Format("list is not UTF-8"))?;
    let mut lines = Vec::new();
    if text.is_empty() {
        return Ok(lines);
    }
    for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        if lines.len() == max_entries {
            return Err(Error::Limit);
        }
        lines.push(line.to_owned());
    }
    Ok(lines)
}

/// Encodes entries as stock newline lists: every entry, empty ones too, ends
/// with `\n` (`0x0069a800`).
pub fn encode_lines<S: AsRef<str>>(entries: &[S]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for entry in entries {
        let entry = entry.as_ref();
        if entry.contains(['\n', '\r', '\0']) {
            return Err(Error::Identity);
        }
        bytes.extend_from_slice(entry.as_bytes());
        bytes.push(b'\n');
    }
    Ok(bytes)
}

/// Decodes a stock `setlist` file. ZenUI accepts only exactly
/// [`SETLIST_SLOTS`] lines (`0x0070c31c`); `empty` (or an empty line) is an
/// unused slot. Any other line count is a format error here; ZenUI resets such
/// a setlist to all-empty slots.
pub fn decode_setlist(bytes: &[u8]) -> Result<Vec<Option<String>>> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Format("setlist is not UTF-8"))?;
    let slots: Vec<Option<String>> = text
        .strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .map(|line| match line {
            EMPTY_SLOT | "" => None,
            path => Some(path.to_owned()),
        })
        .collect();
    if slots.len() != SETLIST_SLOTS {
        return Err(Error::Format("setlist must have exactly 256 lines"));
    }
    Ok(slots)
}

/// Encodes exactly [`SETLIST_SLOTS`] lines, each ended by `\n`.
pub fn encode_setlist(slots: &[Option<String>]) -> Result<Vec<u8>> {
    if slots.len() != SETLIST_SLOTS {
        return Err(Error::Format("setlist must have 256 slots"));
    }
    if slots
        .iter()
        .flatten()
        .any(|path| path.is_empty() || path == EMPTY_SLOT)
    {
        return Err(Error::Identity);
    }
    let lines: Vec<&str> = slots
        .iter()
        .map(|slot| slot.as_deref().unwrap_or(EMPTY_SLOT))
        .collect();
    encode_lines(&lines)
}
