//! The stock CorOS preset library under `/media/p4`, read and written in place.
//!
//! Layout (CorOS 4.1.0):
//!
//! - `Presets/<setlist>/<preset>.pb` + `<preset>.json` sidecar.
//! - `Presets/<setlist>/setlist`: 256 slot lines, a `.pb` device path or `empty`.
//! - `user_preset_folders`: setlist directory order, one device path per line.
//! - `favorites*`, `recents*`: `(folder, item)` line pairs, newest first.
//! - `recentSearches`: one query per line, newest first.
//! - `searchIndexUser` + `searchRecentChangesUser` (see [`SearchHit`]).
//!
//! ZenUI truncates and rewrites these files in place; this library replaces
//! each file atomically instead, with the same bytes. A multi-file change
//! writes new preset files first and the setlist after them, and removes
//! preset files last, only when no slot still names them. The one directory
//! move, [`StockLibrary::rename_setlist`], relies on the slot repair in
//! [`StockLibrary::slots`]: after a crash between its steps every slot still
//! resolves. Favorites and recents may then name old paths; ZenUI skips
//! entries whose file is missing. The search index is a cache: it is updated
//! last, and a failure there never fails a saved change (see [`Saved`]).

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use tetrad_preset::PresetDocument;
use tetrad_storage::media::{
    Collection, MediaFile, MediaRoot, SETLIST_SLOTS, Staged, check_name, decode_lines,
    decode_setlist, encode_lines, encode_setlist, is_temp_file, read_file, write_atomic,
};
use tetrad_storage::search_index::{SearchChanges, SearchIndex};
use tetrad_storage::stock::{Download, DownloadKind, ProductType, StockProduct, StockView};

use crate::sidecar::Sidecar;
use crate::{Error, Result};

mod backup;
#[cfg(test)]
mod backup_tests;
mod captures;
#[cfg(test)]
mod captures_tests;
mod conversion;
mod directory;
mod downloads;
mod legacy;
mod media;
pub mod media_query;
#[cfg(test)]
mod media_tests;
mod plugins;
mod search;
mod upload;

pub use backup::{
    CaptureCiphers, Rekey, Reopened, rekey, rekey_for_backup, reopen_after_restore,
    stage_for_backup,
};
pub use captures::{
    CaptureCheck, CorruptCapture, CorruptionCheck, REKEYED_SUFFIX, quarantine_corrupt_captures,
    restore_quarantined_capture,
};
pub use conversion::{CONVERTED_SUFFIX, LegacyCommit, LegacyConversion};
pub use directory::{
    MAX_MEDIA_FOLDER_DEPTH, MAX_MEDIA_FOLDERS, MoveMode, PLUGIN_DEPTH, PluginFolder, PluginPreset,
    SlotSummary, add_to_media_folder, create_media_folder, delete_media_folder,
    duplicate_media_folder, next_folder_name, rename_media_folder,
};
pub use downloads::{
    CloudCapture, DOWNLOADS_FOLDERS, DownloadKeys, DownloadMove, DownloadsFolder,
    EXCESSIVE_DOWNLOADS, LibraryCapture, StoredDownload, download_row, downloads_folder,
    excessive_downloads, move_download_capture, overwrite_capture_metadata, store_cloud_capture,
    store_download, store_library_capture,
};
pub use legacy::import_legacy_model_defaults;
pub use media::{
    CAPTURE_ROOT_KEY, CloudIr, IR_ROOT_KEY, IrImport, LEGACY_CAPTURE_DIR, LEGACY_SUFFIX,
    LegacyMigration, MEDIA_LIBRARY_CAP, StoredMedia, import_cloud_ir, import_user_ir,
    media_folder_by_key, media_folder_key, rewrite_legacy_ir_references, saving_destination,
    store_media, translate_capture_stores, translate_global_stores,
};
pub use plugins::{PluginFolderChange, StoredPluginPreset};
pub use search::SearchHit;
pub use upload::{CaptureUploadParts, UploadKeys, capture_upload, set_capture_cloud_id};

/// The setlist every stock library has (`/media/p4/Presets/My Presets`).
pub const DEFAULT_SETLIST: &str = "My Presets";
/// User setlists, including the default one (`0x00766764`: count < 12).
pub const MAX_SETLISTS: usize = 12;
/// Entries per favorites or recents file (vtable +0x98 returns 0x40).
pub const MAX_FAVORITES_OR_RECENTS: usize = 64;
/// Entries in `recentSearches` (`0x007a5490`: 0x15).
pub const MAX_RECENT_SEARCHES: usize = 21;
const PRESET_LISTS: [ItemList; 2] = [
    ItemList::Favorites(Collection::Presets),
    ItemList::Recents(Collection::Presets),
];
/// The factory setlist file below `/opt/neuraldsp` (string `0x011f3df0`).
const FACTORY_SETLIST: &str = "Factory Library/setlist";
/// The factory setlist directory below `/opt/neuraldsp`.
const FACTORY_FOLDER: &str = "Factory Library";
/// The slot file inside each setlist directory.
const SETLIST_FILE: &str = "setlist";

/// A folder that stock builds read-only (flag `+8`, `0x7054f0`). A save of a preset loaded
/// from one stamps the account as its author (`0x5b7438`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyFolder {
    /// The Factory Library (`0x76ff74` → `0x7119c4(.., 1, ..)` → `0x705464` flag 1).
    FactoryLibrary,
    /// A factory plugin folder outside its `User` tree (I: the plugin folders `0x215f98`
    /// and `0x214ee8` get flag 1 in `0x76ff74`; which folders those are is not traced).
    FactoryPlugin,
}

/// Read bounds for untrusted file content. Stock list caps are constants.
#[derive(Debug, Clone, Copy)]
pub struct StockLimits {
    pub max_file_bytes: u64,
    pub max_list_entries: usize,
    pub preset: tetrad_preset::Limits,
}

impl Default for StockLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 16 * 1024 * 1024,
            max_list_entries: 100_000,
            preset: tetrad_preset::Limits::default(),
        }
    }
}

/// A `(folder, item)` entry of a favorites or recents file. For presets both
/// are device paths; for captures the folder is a key such as `local_nc_root`
/// and the item is the 64-hex capture id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRef {
    pub folder: String,
    pub item: String,
}

/// A favorites or recents file (`0x007cb024`, `0x007cb54c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemList {
    Favorites(Collection),
    Recents(Collection),
}

impl ItemList {
    fn file(self) -> MediaFile {
        match self {
            Self::Favorites(collection) => MediaFile::Favorites(collection),
            Self::Recents(collection) => MediaFile::Recents(collection),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setlist {
    pub name: String,
    /// Device path of the setlist directory.
    pub path: String,
}

/// A loaded preset file with its exact source bytes, so a no-op save writes
/// nothing.
#[derive(Debug, Clone)]
pub struct StockPreset {
    /// Device path of the `.pb` file.
    pub path: String,
    pub document: PresetDocument,
    pub sidecar: Option<Sidecar>,
    raw: Vec<u8>,
    sidecar_raw: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StockSave {
    Unchanged,
    Written,
}

/// A change that is on disk. `search_index` is set when the search index
/// could not follow it.
#[must_use]
#[derive(Debug)]
pub struct Saved<T> {
    pub value: T,
    pub search_index: Option<StaleSearchIndex>,
}

/// A change that is on disk up to its commit point (review r30 C2): `value` holds, so the
/// caller takes it. A later clean-up step that failed (favorites, recents, old files) is
/// `tail_error`; its leftovers are files or entries that no slot names.
#[must_use]
#[derive(Debug)]
pub struct Committed<T> {
    pub value: T,
    pub search_index: Option<StaleSearchIndex>,
    pub tail_error: Option<Error>,
}

/// The search index is a cache of the setlists. When its update fails after a
/// change is on disk, the change stays and the index files are dropped. The
/// next change, [`StockLibrary::prepare_search_index`] or
/// [`StockLibrary::rebuild_search_index`] rebuilds them, as ZenUI rebuilds a
/// missing index at boot (`0x0073545c`). Searches meanwhile read the
/// setlists directly.
#[derive(Debug, thiserror::Error)]
#[error("search index is stale until rebuilt: {0}")]
pub struct StaleSearchIndex(pub Error);

/// The stock library rooted at a device `/media/p4` or a copy of it.
pub struct StockLibrary {
    media: MediaRoot,
    factory: Option<MediaRoot>,
    limits: StockLimits,
    /// The read-only factory index, decoded once (`None`: no index file).
    factory_index: OnceLock<Option<(SearchIndex, SearchChanges)>>,
    /// Test-only: the step that fails as if power was lost before it.
    #[cfg(test)]
    pub(crate) crash_before: Option<&'static str>,
}

impl StockLibrary {
    /// Opens an existing library: the `Presets` directory must exist.
    pub fn open(media: MediaRoot, limits: StockLimits) -> Result<Self> {
        if !fs::symlink_metadata(media.file(MediaFile::Presets))?.is_dir() {
            return Err(tetrad_storage::Error::NotRegular.into());
        }
        Ok(Self {
            media,
            factory: None,
            limits,
            factory_index: OnceLock::new(),
            #[cfg(test)]
            crash_before: None,
        })
    }

    /// Adds the read-only factory root (`/opt/neuraldsp`): factory presets load
    /// and the factory index joins searches. Factory files are never written.
    pub fn with_factory(mut self, factory: MediaRoot) -> Self {
        self.factory = Some(factory);
        self.factory_index = OnceLock::new();
        self
    }

    pub fn media(&self) -> &MediaRoot {
        &self.media
    }

    /// Fails a multi-step change at `step` in tests (crash injection): every step whose name
    /// starts with `crash_before`.
    fn crash_point(&self, step: &'static str) -> Result<()> {
        #[cfg(test)]
        if self
            .crash_before
            .is_some_and(|before| step.starts_with(before))
        {
            return Err(std::io::Error::other(format!("injected crash before {step}")).into());
        }
        let _ = step;
        Ok(())
    }

    /// The 256 slots of the factory setlist
    /// (`/opt/neuraldsp/Factory Library/setlist`).
    pub fn factory_slots(&self) -> Result<Vec<Option<String>>> {
        let factory = self.factory.as_ref().ok_or(Error::NoFactoryRoot)?;
        let file = factory.resolve(&factory.device_path(FACTORY_SETLIST)?)?;
        let bytes = read_file(&file, self.limits.max_file_bytes)?
            .ok_or_else(|| Error::SetlistMissing(FACTORY_SETLIST.to_owned()))?;
        Ok(decode_setlist(&bytes)?)
    }

    fn read_lines(&self, file: MediaFile) -> Result<Vec<String>> {
        let bytes = read_file(&self.media.file(file), self.limits.max_file_bytes)?;
        Ok(decode_lines(
            bytes.as_deref().unwrap_or_default(),
            self.limits.max_list_entries,
        )?)
    }

    fn write_lines<S: AsRef<str>>(&self, file: MediaFile, lines: &[S]) -> Result<()> {
        Ok(write_atomic(&self.media.file(file), &encode_lines(lines)?)?)
    }

    fn setlist_path(&self, name: &str) -> Result<String> {
        check_name(name)?;
        Ok(self
            .media
            .device_path(&format!("{}/{name}", MediaFile::Presets.relative()))?)
    }

    fn setlist_file(&self, name: &str) -> Result<PathBuf> {
        Ok(self
            .media
            .resolve(&self.setlist_path(name)?)?
            .join(SETLIST_FILE))
    }

    /// Every directory below `Presets` is a setlist (`0x0076cd84`). Order:
    /// the `user_preset_folders` entries first, then unlisted directories by
    /// case-insensitive name (QDir default order; inferred). A listed path
    /// without a directory is ignored.
    pub fn setlists(&self) -> Result<Vec<Setlist>> {
        let mut unlisted = Vec::new();
        for entry in fs::read_dir(self.media.file(MediaFile::Presets))? {
            let entry = entry?;
            let name = entry.file_name().into_string().ok();
            match name {
                Some(name) if entry.file_type()?.is_dir() && check_name(&name).is_ok() => {
                    let path = self.setlist_path(&name)?;
                    unlisted.push(Setlist { name, path });
                }
                _ => {}
            }
        }
        unlisted.sort_by_key(|setlist| setlist.name.to_lowercase());
        let mut ordered = Vec::with_capacity(unlisted.len());
        for path in self.read_lines(MediaFile::UserFolders(Collection::Presets))? {
            if let Some(index) = unlisted.iter().position(|setlist| setlist.path == path) {
                ordered.push(unlisted.remove(index));
            }
        }
        ordered.append(&mut unlisted);
        Ok(ordered)
    }

    /// The 256 slots of a setlist; `None` is an empty slot.
    ///
    /// A setlist directory without a `setlist` file reads as empty: ZenUI
    /// resets a setlist file it cannot use (`0x0070c31c`; inferred for a
    /// missing file). A slot whose path lies outside this setlist directory
    /// reads as `<setlist dir>/<file name>` when that file exists: the safe
    /// subset of the stock repair `0x0070dbc0`, which Tetrad applies in
    /// memory only (stock also copies a missing file over). The next write of
    /// the setlist stores the repair.
    pub fn slots(&self, setlist: &str) -> Result<Vec<Option<String>>> {
        let directory = self.setlist_path(setlist)?;
        let host = self.media.resolve(&directory)?;
        let mut slots = match read_file(&host.join(SETLIST_FILE), self.limits.max_file_bytes)? {
            Some(bytes) => decode_setlist(&bytes)?,
            None if fs::symlink_metadata(&host).is_ok_and(|meta| meta.is_dir()) => {
                vec![None; SETLIST_SLOTS]
            }
            None => return Err(Error::SetlistMissing(setlist.to_owned())),
        };
        for path in slots.iter_mut().flatten() {
            let Some((parent, file)) = path.rsplit_once('/') else {
                continue;
            };
            if parent == directory {
                continue;
            }
            let local = format!("{directory}/{file}");
            let exists = self
                .media
                .resolve(&local)
                .is_ok_and(|host| fs::symlink_metadata(host).is_ok_and(|meta| meta.is_file()));
            if exists {
                *path = local;
            }
        }
        Ok(slots)
    }

    fn write_slots(&self, setlist: &str, slots: &[Option<String>]) -> Result<()> {
        Ok(write_atomic(
            &self.setlist_file(setlist)?,
            &encode_setlist(slots)?,
        )?)
    }

    /// True when a slot of any user setlist names `path`. A setlist that
    /// cannot be read counts as naming it, so its files are kept.
    fn referenced(&self, path: &str) -> bool {
        let Ok(setlists) = self.setlists() else {
            return true;
        };
        setlists.iter().any(|setlist| {
            self.slots(&setlist.name).map_or(true, |slots| {
                slots.iter().flatten().any(|slot| slot == path)
            })
        })
    }

    /// Creates an empty setlist directory and file (`0x0076dd8c`), then
    /// registers it last in `user_preset_folders`. At most [`MAX_SETLISTS`].
    pub fn create_setlist(&mut self, name: &str) -> Result<Setlist> {
        let path = self.setlist_path(name)?;
        if self.setlists()?.len() >= MAX_SETLISTS {
            return Err(Error::SetlistsFull);
        }
        if fs::symlink_metadata(self.media.resolve(&path)?).is_ok() {
            return Err(Error::SetlistExists(name.to_owned()));
        }
        let mut folders = self.read_lines(MediaFile::UserFolders(Collection::Presets))?;
        // A listed path without a directory is left by an interrupted delete.
        folders.retain(|folder| *folder != path);
        fs::create_dir(self.media.resolve(&path)?)?;
        self.write_slots(name, &vec![None; SETLIST_SLOTS])?;
        folders.push(path.clone());
        self.write_lines(MediaFile::UserFolders(Collection::Presets), &folders)?;
        Ok(Setlist {
            name: name.to_owned(),
            path,
        })
    }

    /// Renames a setlist directory and rewrites every stored path below it:
    /// its slots, `user_preset_folders`, preset favorites and recents, then
    /// the search index. A crash after the directory move leaves slots that
    /// [`Self::slots`] repairs.
    pub fn rename_setlist(&mut self, from: &str, to: &str) -> Result<Saved<()>> {
        let old = self.setlist_path(from)?;
        let new = self.setlist_path(to)?;
        if fs::symlink_metadata(self.media.resolve(&new)?).is_ok() {
            return Err(Error::SetlistExists(to.to_owned()));
        }
        let mut folders = self.read_lines(MediaFile::UserFolders(Collection::Presets))?;
        let slots = self.slots(from)?;
        let moved = |path: &str| match path.strip_prefix(&old) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{new}{rest}"),
            _ => path.to_owned(),
        };
        fs::rename(self.media.resolve(&old)?, self.media.resolve(&new)?)?;
        self.crash_point("rename_setlist: slots")?;
        let renamed: Vec<_> = slots
            .iter()
            .map(|slot| slot.as_deref().map(moved))
            .collect();
        self.write_slots(to, &renamed)?;
        self.crash_point("rename_setlist: folders")?;
        // An unlisted setlist directory becomes listed under its new path.
        folders.retain(|folder| *folder != new);
        match folders.iter().position(|folder| *folder == old) {
            Some(position) => folders[position].clone_from(&new),
            None => folders.push(new.clone()),
        }
        self.write_lines(MediaFile::UserFolders(Collection::Presets), &folders)?;
        self.crash_point("rename_setlist: items")?;
        // The folder-rename fix-up of `0x007cccc8`.
        for list in PRESET_LISTS {
            let lines: Vec<_> = self
                .read_lines(list.file())?
                .iter()
                .map(|line| moved(line))
                .collect();
            self.write_lines(list.file(), &lines)?;
        }
        let removed: Vec<&str> = slots.iter().flatten().map(String::as_str).collect();
        let added: Vec<&str> = renamed.iter().flatten().map(String::as_str).collect();
        Ok(Saved {
            value: (),
            search_index: self.update_search_index(&added, &removed),
        })
    }

    /// Deletes an empty setlist (every slot empty). The default setlist stays.
    ///
    /// Stock removes the directory recursively (`0x00699874`). Tetrad first
    /// checks that the directory holds only its `setlist` file and temporary
    /// files a crash left ([`is_temp_file`]), and removes only those. Any other
    /// entry may be a preset file that a slot elsewhere still names, so the
    /// delete is refused with [`Error::SetlistHasFiles`] and nothing changes.
    /// `user_preset_folders` is updated last.
    pub fn delete_setlist(&mut self, name: &str) -> Result<()> {
        if name == DEFAULT_SETLIST {
            return Err(Error::Unsupported("the default setlist cannot be deleted"));
        }
        if self.slots(name)?.iter().any(Option::is_some) {
            return Err(Error::SetlistNotEmpty(name.to_owned()));
        }
        let path = self.setlist_path(name)?;
        let directory = self.media.resolve(&path)?;
        let (mut removable, mut extra) = (Vec::new(), Vec::new());
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_file() && (file == SETLIST_FILE || is_temp_file(&file)) {
                removable.push(entry.path());
            } else {
                extra.push(file);
            }
        }
        if !extra.is_empty() {
            extra.sort();
            return Err(Error::SetlistHasFiles {
                setlist: name.to_owned(),
                files: extra,
            });
        }
        for file in removable {
            fs::remove_file(file)?;
        }
        fs::remove_dir(directory)?;
        let folders: Vec<_> = self
            .read_lines(MediaFile::UserFolders(Collection::Presets))?
            .into_iter()
            .filter(|folder| *folder != path)
            .collect();
        self.write_lines(MediaFile::UserFolders(Collection::Presets), &folders)
    }

    /// Loads a `.pb` preset and its optional `.json` sidecar.
    pub fn load(&self, path: &str) -> Result<StockPreset> {
        let file = self.preset_file_in(path, true)?;
        let raw = read_file(&file, self.limits.max_file_bytes)?
            .ok_or_else(|| Error::PresetMissing(path.to_owned()))?;
        let document = PresetDocument::parse(&raw, self.limits.preset)?;
        let sidecar_raw = read_file(&file.with_extension("json"), self.limits.max_file_bytes)?;
        let sidecar = sidecar_raw.as_deref().map(Sidecar::parse).transpose()?;
        Ok(StockPreset {
            path: path.to_owned(),
            document,
            sidecar,
            raw,
            sidecar_raw,
        })
    }

    /// The `.json` sidecar of a user or factory `.pb` preset; `None` when it is absent.
    pub fn sidecar(&self, path: &str) -> Result<Option<Sidecar>> {
        let file = self.preset_file_in(path, true)?.with_extension("json");
        read_file(&file, self.limits.max_file_bytes)?
            .as_deref()
            .map(Sidecar::parse)
            .transpose()
    }

    /// The read-only folder (stock folder flag `+8`, `0x7054f0`) that holds the preset
    /// `path`; `None` for a writable one.
    pub fn read_only_folder(&self, path: &str) -> Option<ReadOnlyFolder> {
        let rest = path.strip_prefix(self.factory.as_ref()?.device())?;
        match rest.strip_prefix("/Plugins/") {
            Some(plugin) => match plugin.split_once('/') {
                Some((_, tail)) if tail.starts_with("User/") => None,
                _ => Some(ReadOnlyFolder::FactoryPlugin),
            },
            None => (rest.strip_prefix('/')?.split_once('/')?.0 == FACTORY_FOLDER)
                .then_some(ReadOnlyFolder::FactoryLibrary),
        }
    }

    /// Resolves a user preset path, or a factory one when `factory` is allowed.
    fn preset_file_in(&self, path: &str, factory: bool) -> Result<PathBuf> {
        if !path.ends_with(".pb") {
            return Err(tetrad_storage::Error::DevicePath(path.to_owned()).into());
        }
        match &self.factory {
            Some(root) if root.contains(path) => {
                if factory {
                    Ok(root.resolve(path)?)
                } else {
                    Err(tetrad_storage::Error::ReadOnly.into())
                }
            }
            _ => Ok(self.media.resolve(path)?),
        }
    }

    fn preset_file(&self, path: &str) -> Result<PathBuf> {
        self.preset_file_in(path, false)
    }

    /// Writes changed preset bytes, then a changed sidecar. The file name and
    /// slot stay (use [`Self::rename_preset`] to rename). No-op saves write
    /// nothing.
    pub fn save(&mut self, preset: &mut StockPreset) -> Result<StockSave> {
        let file = self.preset_file(&preset.path)?;
        let raw = preset.document.to_bytes();
        let sidecar = preset.sidecar.as_ref().map(Sidecar::encode);
        let sidecar_changed = match (&preset.sidecar, &preset.sidecar_raw) {
            (Some(current), Some(original)) => Sidecar::parse(original)? != *current,
            (current, original) => current.is_some() != original.is_some(),
        };
        if raw == preset.raw && !sidecar_changed {
            return Ok(StockSave::Unchanged);
        }
        if raw != preset.raw {
            write_atomic(&file, &raw)?;
            preset.raw = raw;
        }
        if sidecar_changed {
            let json = file.with_extension("json");
            match sidecar {
                Some(bytes) => {
                    write_atomic(&json, &bytes)?;
                    preset.sidecar_raw = Some(bytes);
                }
                None => {
                    fs::remove_file(json)?;
                    preset.sidecar_raw = None;
                }
            }
        }
        Ok(StockSave::Written)
    }

    /// A free `<directory>/<name>.pb` path by ZenUI's rule (`0x00699f18`):
    /// while the file exists, a numeric `_<n>` suffix becomes `_<n+1>`, else
    /// `_1` is appended. The 20-character trim ZenUI then applies is not
    /// recovered exactly and is not applied.
    fn free_preset_path(&self, directory: &str, name: &str) -> Result<String> {
        let mut base = name.to_owned();
        for _ in 0..=SETLIST_SLOTS * 4 {
            check_name(&format!("{base}.pb"))?;
            let path = format!("{directory}/{base}.pb");
            if fs::symlink_metadata(self.media.resolve(&path)?).is_err() {
                return Ok(path);
            }
            base = match base.rsplit_once('_') {
                Some((stem, number)) => match number.parse::<u32>() {
                    Ok(n) => format!("{stem}_{}", n.saturating_add(1)),
                    Err(_) => format!("{base}_1"),
                },
                None => format!("{base}_1"),
            };
        }
        Err(tetrad_storage::Error::Limit.into())
    }

    /// Stores a new preset file and its sidecar (ZenUI always writes one; see
    /// [`Sidecar::new_preset`]) in an empty slot, named after the preset by
    /// `free_preset_path`. The setlist is written after the files. Returns the
    /// new path.
    pub fn store(
        &mut self,
        setlist: &str,
        slot: usize,
        name: &str,
        document: &PresetDocument,
        sidecar: &Sidecar,
    ) -> Result<Saved<String>> {
        let mut slots = self.slots(setlist)?;
        match slots.get(slot) {
            None => return Err(Error::SlotRange(slot)),
            Some(Some(_)) => return Err(Error::SlotOccupied(slot)),
            Some(None) => {}
        }
        let path = self.free_preset_path(&self.setlist_path(setlist)?, name)?;
        let file = self.media.resolve(&path)?;
        write_atomic(&file, &document.to_bytes())?;
        write_atomic(&file.with_extension("json"), &sidecar.encode())?;
        slots[slot] = Some(path.clone());
        self.write_slots(setlist, &slots)?;
        let search_index = self.update_search_index(&[&path], &[]);
        Ok(Saved {
            value: path,
            search_index,
        })
    }

    /// Stores a preset into a slot and replaces what the slot holds (review r20 G4): the new
    /// files are written first, then the slot points at them, then the old preset goes as
    /// [`Self::delete`] does (its favorites and recents, and its files when no other slot
    /// names them). A write error leaves the slot and the old files as they were: both new
    /// files are staged before the sidecar, then the document, is renamed. The file is named
    /// after the preset (`free_preset_path`); when the old file is `<name>.pb` and only this
    /// slot names it, it is written over in place. An empty slot is [`Self::store`]. A
    /// failed clean-up after the slot names the new file (favorites, recents, old files) is
    /// an `Err` here; [`Self::store_cloud_preset`] reports it as a `tail_error`.
    pub fn replace(
        &mut self,
        setlist: &str,
        slot: usize,
        name: &str,
        document: &PresetDocument,
        sidecar: &Sidecar,
    ) -> Result<Saved<String>> {
        let replaced = self.replace_committed(setlist, slot, name, document, sidecar, false)?;
        match replaced.tail_error {
            Some(error) => Err(error),
            None => Ok(Saved {
                value: replaced.value,
                search_index: replaced.search_index,
            }),
        }
    }

    /// [`Self::replace`] up to its commit point (the slot names the new file); `rehash` sets
    /// the preset hash of an occupied slot's new file to the id of its path (`0x21ee8c`). A
    /// failed setlist write drops the new files again (they were at a free path).
    fn replace_committed(
        &mut self,
        setlist: &str,
        slot: usize,
        name: &str,
        document: &PresetDocument,
        sidecar: &Sidecar,
        rehash: bool,
    ) -> Result<Committed<String>> {
        let mut slots = self.slots(setlist)?;
        let old = match slots.get(slot) {
            None => return Err(Error::SlotRange(slot)),
            Some(None) => {
                let saved = self.store(setlist, slot, name, document, sidecar)?;
                return Ok(Committed {
                    value: saved.value,
                    search_index: saved.search_index,
                    tail_error: None,
                });
            }
            Some(Some(old)) => old.clone(),
        };
        let old_file = self.preset_file(&old)?;
        let in_place = old == format!("{}/{name}.pb", self.setlist_path(setlist)?)
            && !self.named_by_another_slot(&old, setlist, slot);
        let path = match in_place {
            true => old.clone(),
            false => self.free_preset_path(&self.setlist_path(setlist)?, name)?,
        };
        let file = self.media.resolve(&path)?;
        let json = file.with_extension("json");
        let bytes = match rehash {
            true => hashed(document, &path)?.to_bytes(),
            false => document.to_bytes(),
        };
        // Both files are staged before either is renamed (review-r21 H5): a write error
        // changes nothing. The document goes first (review r33 F4): a failed document commit
        // also changes nothing, and after a failed sidecar commit the old document comes back
        // in place, so a sidecar never names (with its `cloudId`) a document it did not come
        // with.
        let old_document = match in_place {
            true => Some(fs::read(&file)?),
            false => None,
        };
        let staged_sidecar = Staged::write(&json, &sidecar.encode())?;
        Staged::write(&file, &bytes)?.commit()?;
        if let Err(error) = staged_sidecar.commit() {
            // Best effort: in place the old bytes go back; else the path was free, so the
            // document is ours.
            let _ = match old_document {
                Some(old) => Staged::write(&file, &old).and_then(Staged::commit),
                None => fs::remove_file(&file).map_err(Into::into),
            };
            return Err(error.into());
        }
        if !in_place {
            slots[slot] = Some(path.clone());
            if let Err(error) = self.write_slots(setlist, &slots) {
                let _ = remove_preset_files(&file);
                return Err(error);
            }
        }
        let unnamed = !in_place && !self.referenced(&old);
        let tail = (|| {
            if in_place || unnamed {
                self.forget_items(&old)?;
            }
            if unnamed {
                remove_preset_files(&old_file)?;
            }
            Ok(())
        })();
        let removed: &[&str] = if unnamed { &[&old] } else { &[] };
        Ok(Committed {
            search_index: self.update_search_index(&[&path], removed),
            value: path,
            tail_error: tail.err(),
        })
    }

    /// Whether a slot other than `slot` of `setlist` names `path`. Unreadable setlists count
    /// as naming it, as in [`Self::referenced`].
    fn named_by_another_slot(&self, path: &str, setlist: &str, slot: usize) -> bool {
        let Ok(setlists) = self.setlists() else {
            return true;
        };
        setlists.iter().any(|other| {
            self.slots(&other.name).map_or(true, |slots| {
                slots.iter().enumerate().any(|(index, named)| {
                    named.as_deref() == Some(path) && (other.name != setlist || index != slot)
                })
            })
        })
    }

    /// Points the favorites and recents that name `old` at `new`, for each `(old, new, folder)`;
    /// `folder` replaces the entry's folder when it is given. A list that holds no such entry
    /// is not written.
    fn follow_items(&mut self, moves: &[(&str, &str, Option<&str>)]) -> Result<()> {
        for list in PRESET_LISTS {
            let entries = self.items(list)?;
            let moved: Vec<_> = (entries.iter())
                .map(
                    |entry| match moves.iter().find(|(old, ..)| entry.item == *old) {
                        Some((_, new, folder)) => ItemRef {
                            folder: folder.map_or_else(|| entry.folder.clone(), str::to_owned),
                            item: (*new).to_owned(),
                        },
                        None => entry.clone(),
                    },
                )
                .collect();
            if moved != entries {
                self.write_items(list, &moved)?;
            }
        }
        Ok(())
    }

    /// Drops the favorites and recents that name `path`.
    fn forget_items(&mut self, path: &str) -> Result<()> {
        for list in PRESET_LISTS {
            let kept: Vec<_> = self
                .items(list)?
                .into_iter()
                .filter(|entry| entry.item != path)
                .collect();
            self.write_items(list, &kept)?;
        }
        Ok(())
    }

    /// Renames the preset in a slot. Files are named after presets, so the
    /// renamed document and sidecar go to a free `<name>.pb` first; then the
    /// slot points at it. When no other slot names the old path, favorites and
    /// recents follow and the old files go (inferred from the stock naming
    /// rule). Returns the new path.
    pub fn rename_preset(
        &mut self,
        setlist: &str,
        slot: usize,
        name: &str,
    ) -> Result<Saved<String>> {
        let mut slots = self.slots(setlist)?;
        let old = slots
            .get(slot)
            .ok_or(Error::SlotRange(slot))?
            .clone()
            .ok_or(Error::SlotEmpty(slot))?;
        let mut preset = self.load(&old)?;
        preset.document.rename(name)?;
        preset
            .sidecar
            .get_or_insert_with(Sidecar::default)
            .set_text("name", name);
        if old == format!("{}/{name}.pb", self.setlist_path(setlist)?) {
            self.save(&mut preset)?;
            return Ok(Saved {
                value: old,
                search_index: None,
            });
        }
        let old_file = self.preset_file(&old)?;
        let new = self.free_preset_path(&self.setlist_path(setlist)?, name)?;
        let new_file = self.media.resolve(&new)?;
        write_atomic(&new_file, &preset.document.to_bytes())?;
        if let Some(sidecar) = &preset.sidecar {
            write_atomic(&new_file.with_extension("json"), &sidecar.encode())?;
        }
        slots[slot] = Some(new.clone());
        self.write_slots(setlist, &slots)?;
        if self.referenced(&old) {
            let search_index = self.update_search_index(&[&new], &[]);
            return Ok(Saved {
                value: new,
                search_index,
            });
        }
        for list in PRESET_LISTS {
            let entries: Vec<_> = self
                .items(list)?
                .into_iter()
                .map(|mut entry| {
                    if entry.item == old {
                        entry.item.clone_from(&new);
                    }
                    entry
                })
                .collect();
            self.write_items(list, &entries)?;
        }
        remove_preset_files(&old_file)?;
        let search_index = self.update_search_index(&[&new], &[&old]);
        Ok(Saved {
            value: new,
            search_index,
        })
    }

    /// Swaps two slots of one setlist (a move onto an empty slot).
    pub fn swap_slots(&mut self, setlist: &str, a: usize, b: usize) -> Result<()> {
        let mut slots = self.slots(setlist)?;
        if a >= SETLIST_SLOTS || b >= SETLIST_SLOTS {
            return Err(Error::SlotRange(a.max(b)));
        }
        slots.swap(a, b);
        self.write_slots(setlist, &slots)
    }

    /// Exchanges the presets of slot `slot_a` of setlist `a` and slot `slot_b` of setlist `b`
    /// (copy-swap `0x1e5a58` → `ContentController::swapInLocalFolder` `0x1e3824`). Each
    /// preset becomes a new file in the other setlist's directory (a free `<name>.pb`, its hash
    /// the id of the new path, `0x21ee8c`); the two slots name the new files; the old files go
    /// when no slot names them, and their favorites and recents follow to the new paths.
    ///
    /// Stock deletes both old files first (`0x70c47c` → `doRemoveFile` `0x21e67c`), then writes
    /// the new ones (`0x70e61c` → `0x21ee8c`). Tetrad writes first: both new files, then setlist
    /// `a`, then setlist `b`; an error at `b` drops the new file of `b` (review r30 C3: it frees
    /// space for the next write), writes `a` back and drops the new file of `a`, so both presets
    /// stay where they were. When `a` cannot be written back, [`Error::SwapIncomplete`] names
    /// the files that no slot names now. The old files go last. A crash between the two setlist
    /// writes leaves one preset in both slots and the other one in its old and new files, named
    /// by no slot. Names stay (O: stock names the copy after its new file, `0x21efd8`, which
    /// write-first would turn into `<name>_1` while the old file still exists).
    ///
    /// Returns the new path of each preset: first the preset of `a` (now in `b`), then the
    /// preset of `b` (now in `a`). After the write of `b` the swap is done: a failed favorites,
    /// recents or old-file step is [`Committed::tail_error`] (review r30 C2). Both slots must
    /// hold a preset; one setlist is [`Self::move_slot`].
    pub fn swap_between(
        &mut self,
        a: &str,
        slot_a: usize,
        b: &str,
        slot_b: usize,
    ) -> Result<Committed<[String; 2]>> {
        if a == b {
            return Err(Error::Unsupported(
                "a swap between two slots of one setlist",
            ));
        }
        let held = |slots: &[Option<String>], slot: usize| {
            slots
                .get(slot)
                .ok_or(Error::SlotRange(slot))?
                .clone()
                .ok_or(Error::SlotEmpty(slot))
        };
        let (before_a, mut slots_b) = (self.slots(a)?, self.slots(b)?);
        let (old_a, old_b) = (held(&before_a, slot_a)?, held(&slots_b, slot_b)?);
        let (preset_a, preset_b) = (self.load(&old_a)?, self.load(&old_b)?);
        let (dir_a, dir_b) = (self.setlist_path(a)?, self.setlist_path(b)?);
        let into_a = self.stage_new(&dir_a, &preset_b)?;
        let into_b = self.stage_new(&dir_b, &preset_a)?;
        let (new_a, file_a) = into_a.commit()?;
        let (new_b, file_b) = match into_b.commit() {
            Ok(committed) => committed,
            Err(error) => {
                let _ = remove_preset_files(&file_a);
                return Err(error);
            }
        };
        let mut slots_a = before_a.clone();
        slots_a[slot_a] = Some(new_a.clone());
        slots_b[slot_b] = Some(new_b.clone());
        if let Err(error) = self.write_slots(a, &slots_a) {
            let _ = remove_preset_files(&file_a);
            let _ = remove_preset_files(&file_b);
            return Err(error);
        }
        let second =
            (self.crash_point("swap_between: b")).and_then(|()| self.write_slots(b, &slots_b));
        if let Err(error) = second {
            let dropped_b = remove_preset_files(&file_b).is_ok();
            let back = (self.crash_point("swap_between: a back"))
                .and_then(|()| self.write_slots(a, &before_a));
            if back.is_ok() {
                let _ = remove_preset_files(&file_a);
                return Err(error);
            }
            // `a` names the copy of `b`: preset `a` (and the copy of it, when it could not be
            // dropped) is in no setlist now.
            let orphans = std::iter::once(old_a).chain((!dropped_b).then_some(new_b));
            return Err(Error::SwapIncomplete {
                error: Box::new(error),
                orphans: orphans.collect(),
            });
        }
        let mut removed = Vec::new();
        let moves = [(&old_a, &new_b, &dir_b), (&old_b, &new_a, &dir_a)];
        let tail_error = self.swap_tail(moves, &mut removed).err();
        let removed: Vec<&str> = removed.iter().map(String::as_str).collect();
        let search_index = self.update_search_index(&[&new_a, &new_b], &removed);
        Ok(Committed {
            value: [new_b, new_a],
            search_index,
            tail_error,
        })
    }

    /// The steps of [`Self::swap_between`] after both setlist writes, for each `(old, new,
    /// new folder)`: when no slot names the old file, its favorites and recents follow the new
    /// file (`0x1e3824` adds the new record to the favorites when the old one was there), then
    /// the old files go and their path is pushed to `removed`.
    fn swap_tail(
        &mut self,
        moves: [(&String, &String, &String); 2],
        removed: &mut Vec<String>,
    ) -> Result<()> {
        let gone = moves.map(|(old, ..)| !self.referenced(old));
        let follow: Vec<_> = (moves.iter().zip(gone))
            .filter(|(_, gone)| *gone)
            .map(|((old, new, folder), _)| (old.as_str(), new.as_str(), Some(folder.as_str())))
            .collect();
        self.follow_items(&follow)?;
        for ((old, ..), gone) in moves.into_iter().zip(gone) {
            if gone {
                remove_preset_files(&self.preset_file(old)?)?;
                removed.push(old.clone());
            }
        }
        Ok(())
    }

    /// Stages `preset` as a new file in `directory`: a free `<name>.pb` (`free_preset_path`)
    /// named after the preset ([`stored_name`]), its hash the id of that path (`0x21ee8c` →
    /// `0x2e1244`), and its sidecar (stock always writes one, `0x21eda0`). Nothing is renamed
    /// into place yet.
    fn stage_new(&self, directory: &str, preset: &StockPreset) -> Result<NewPreset> {
        let sidecar = preset.sidecar.clone().unwrap_or_default();
        let name = stored_name(&preset.document, Some(&sidecar))
            .unwrap_or_else(|| stem(&preset.path).to_owned());
        self.stage_named(directory, &name, &preset.document, &sidecar)
    }

    fn stage_named(
        &self,
        directory: &str,
        name: &str,
        document: &PresetDocument,
        sidecar: &Sidecar,
    ) -> Result<NewPreset> {
        let path = self.free_preset_path(directory, name)?;
        let file = self.media.resolve(&path)?;
        let document = Staged::write(&file, &hashed(document, &path)?.to_bytes())?;
        let sidecar = Staged::write(&file.with_extension("json"), &sidecar.encode())?;
        Ok(NewPreset {
            path,
            file,
            document,
            sidecar,
        })
    }

    /// Empties a slot. When no other slot names the same path, favorites and
    /// recents that name it are dropped and its preset files are deleted.
    pub fn delete(&mut self, setlist: &str, slot: usize) -> Result<Saved<()>> {
        let mut slots = self.slots(setlist)?;
        let path = slots
            .get_mut(slot)
            .ok_or(Error::SlotRange(slot))?
            .take()
            .ok_or(Error::SlotEmpty(slot))?;
        let file = self.preset_file(&path)?;
        self.write_slots(setlist, &slots)?;
        if self.referenced(&path) {
            return Ok(Saved {
                value: (),
                search_index: None,
            });
        }
        self.forget_items(&path)?;
        remove_preset_files(&file)?;
        Ok(Saved {
            value: (),
            search_index: self.update_search_index(&[], &[&path]),
        })
    }

    /// Entries of a favorites or recents file, newest first. Entries whose
    /// target no longer exists are returned too; ZenUI skips them when shown.
    /// A trailing odd line is left out and the pairs before it are kept, as
    /// stock (`0x007d0224` only logs it).
    pub fn items(&self, list: ItemList) -> Result<Vec<ItemRef>> {
        let lines = self.read_lines(list.file())?;
        Ok(lines
            .chunks_exact(2)
            .map(|pair| ItemRef {
                folder: pair[0].clone(),
                item: pair[1].clone(),
            })
            .collect())
    }

    /// Whether the folder key of a preset favorite or recent names a directory of the
    /// media or factory root. Stock skips an entry whose folder is not in its tree
    /// (`0x007d0224` → `0x0076bc68`); preset folder keys are directory paths.
    pub fn preset_folder_exists(&self, folder: &str) -> bool {
        let root = match &self.factory {
            Some(root) if root.contains(folder) => root,
            _ => &self.media,
        };
        root.resolve(folder).is_ok_and(|host| host.is_dir())
    }

    fn write_items(&self, list: ItemList, entries: &[ItemRef]) -> Result<()> {
        let lines: Vec<&str> = entries
            .iter()
            .flat_map(|entry| [entry.folder.as_str(), entry.item.as_str()])
            .collect();
        self.write_lines(list.file(), &lines)
    }

    /// Adds a favorite or recent the stock way (`0x007ceb78`): drop an equal
    /// entry, prepend, keep at most [`MAX_FAVORITES_OR_RECENTS`].
    pub fn push_item(&mut self, list: ItemList, entry: &ItemRef) -> Result<()> {
        let mut entries = self.items(list)?;
        entries.retain(|existing| existing != entry);
        entries.insert(0, entry.clone());
        entries.truncate(MAX_FAVORITES_OR_RECENTS);
        self.write_items(list, &entries)
    }

    /// `0x007cd32c`. Removing an absent entry writes nothing.
    pub fn remove_item(&mut self, list: ItemList, entry: &ItemRef) -> Result<()> {
        let mut entries = self.items(list)?;
        let count = entries.len();
        entries.retain(|existing| existing != entry);
        if entries.len() == count {
            return Ok(());
        }
        self.write_items(list, &entries)
    }

    /// Newest first.
    pub fn recent_searches(&self) -> Result<Vec<String>> {
        self.read_lines(MediaFile::RecentSearches)
    }

    /// `0x007a5490`: drop equal (case-sensitive) queries, prepend, keep at
    /// most [`MAX_RECENT_SEARCHES`].
    pub fn push_recent_search(&mut self, query: &str) -> Result<()> {
        let mut queries = self.recent_searches()?;
        queries.retain(|existing| existing != query);
        queries.insert(0, query.to_owned());
        queries.truncate(MAX_RECENT_SEARCHES);
        self.write_lines(MediaFile::RecentSearches, &queries)
    }

    /// `0x007a5da4` deletes the file.
    pub fn clear_recent_searches(&mut self) -> Result<()> {
        remove_if_present(&self.media.file(MediaFile::RecentSearches))
    }

    /// The factory reset of the user files (`0x00770334`): remove
    /// `user_preset_folders` and the six favorites and recents files, then the
    /// `Presets` and `Plugins` trees (`0x00699874`). The reload makes
    /// `Presets/My Presets` again (`0x0076cf20`) and one `Plugins/<plugin>`
    /// directory per factory plugin (`0x0076e430`: the factory `User` links
    /// point there). The user search index is rebuilt empty (`0x007699e8`);
    /// its files go first, because [`Self::rebuild_search_index`] keeps earlier
    /// capture entries and the captures go with `zenui.db`, which the caller
    /// recreates. `recentSearches` is a separate reset step.
    pub fn factory_reset(&mut self) -> Result<Saved<()>> {
        remove_if_present(&self.media.file(MediaFile::UserFolders(Collection::Presets)))?;
        for collection in [Collection::Presets, Collection::Captures, Collection::Irs] {
            for list in [
                ItemList::Favorites(collection),
                ItemList::Recents(collection),
            ] {
                remove_if_present(&self.media.file(list.file()))?;
            }
        }
        let plugins = self.media.resolve(&self.media.device_path("Plugins")?)?;
        for tree in [self.media.file(MediaFile::Presets), plugins.clone()] {
            match fs::remove_dir_all(&tree) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            }
        }
        fs::create_dir_all(self.media.resolve(&self.setlist_path(DEFAULT_SETLIST)?)?)?;
        if let Some(factory) = &self.factory {
            let host = factory.resolve(&factory.device_path("Plugins")?)?;
            for (name, _) in directory::named_entries(&host, true)? {
                fs::create_dir_all(plugins.join(name))?;
            }
        }
        for file in [
            MediaFile::SearchIndexUser,
            MediaFile::SearchRecentChangesUser,
        ] {
            remove_if_present(&self.media.file(file))?;
        }
        Ok(Saved {
            value: (),
            search_index: self.rebuild_search_index().err().map(StaleSearchIndex),
        })
    }
}

fn remove_if_present(file: &std::path::Path) -> Result<()> {
    match fs::remove_file(file) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Removes a `.pb` file and its sidecar; absent files are fine.
fn remove_preset_files(file: &std::path::Path) -> Result<()> {
    remove_if_present(&file.with_extension("json"))?;
    remove_if_present(file)
}

/// A preset staged at a free path by `StockLibrary::stage_new`.
struct NewPreset {
    path: String,
    file: PathBuf,
    document: Staged,
    sidecar: Staged,
}

impl NewPreset {
    /// Renames the sidecar, then the document, into place; a failed document drops the
    /// sidecar again (the path was free, so it is ours). Returns the device and host paths.
    fn commit(self) -> Result<(String, PathBuf)> {
        self.sidecar.commit()?;
        if let Err(error) = self.document.commit() {
            let _ = fs::remove_file(self.file.with_extension("json"));
            return Err(error.into());
        }
        Ok((self.path, self.file))
    }
}

/// `document` with its hash set to the id of `path` (FNV-1a, plus `0x2000` when below it): stock
/// stamps it whenever it writes a preset to a new path (`0x21ee8c`, `0x24db44`).
fn hashed(document: &PresetDocument, path: &str) -> Result<PresetDocument> {
    let mut document = document.clone();
    document.modify(|preset| {
        preset.hash = Some(tetrad_core::PresetId::from_identifier(path).0);
        Ok(())
    })?;
    Ok(document)
}

/// The name a stock record carries (`+0x10`): the sidecar `name` (`0x7efc38`), else the name
/// stored in the preset; `None` when both are empty.
fn stored_name(document: &PresetDocument, sidecar: Option<&Sidecar>) -> Option<String> {
    let named = sidecar.and_then(|sidecar| sidecar.text("name"));
    match named.filter(|name| !name.is_empty()) {
        Some(name) => Some(name.to_owned()),
        None => document
            .summary()
            .ok()?
            .name
            .filter(|name| !name.is_empty()),
    }
}

/// The file stem of a `.pb` device path.
fn stem(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.strip_suffix(".pb").unwrap_or(file)
}

/// Where a capture key resolves (`local_nc_root` items, preset references).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureSource {
    /// A user capture: `product` type 2 with this id.
    User(StockProduct),
    /// A cloud download whose cloud id or capture key matches.
    Download(Download),
    /// A read-only capture in `factory.db`.
    Factory(StockProduct),
}

/// Resolves a capture key the way ZenUI searches its stores: user products,
/// then downloads (`id OR hash`), then the factory database.
pub fn find_capture(
    user: &StockView<'_>,
    factory: Option<&StockView<'_>>,
    key: &str,
) -> Result<Option<CaptureSource>> {
    let capture = |product: Option<StockProduct>| {
        product.filter(|product| product.product_type == ProductType::Capture)
    };
    if let Some(product) = capture(user.product(key)?) {
        return Ok(Some(CaptureSource::User(product)));
    }
    if let Some(download) = user.download(DownloadKind::Capture, key)? {
        return Ok(Some(CaptureSource::Download(download)));
    }
    Ok(match factory {
        Some(factory) => capture(factory.product(key)?).map(CaptureSource::Factory),
        None => None,
    })
}

/// Loads a cloud-downloaded preset from `downloads_product_preset`: rawdata is
/// the preset protobuf and metadata the same JSON as a `.json` sidecar.
pub fn load_download(
    user: &StockView<'_>,
    id: &str,
    limits: tetrad_preset::Limits,
) -> Result<Option<(PresetDocument, Option<Sidecar>)>> {
    let Some(download) = user.download(DownloadKind::Preset, id)? else {
        return Ok(None);
    };
    let raw = download
        .raw_data
        .ok_or_else(|| Error::PresetDataMissing(id.to_owned()))?;
    let sidecar = download
        .metadata
        .as_deref()
        .map(Sidecar::parse)
        .transpose()?;
    Ok(Some((PresetDocument::parse(&raw, limits)?, sidecar)))
}
