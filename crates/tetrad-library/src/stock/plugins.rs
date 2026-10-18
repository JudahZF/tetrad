//! Plugin preset writes: the Cortex Control plugin import (`0x24db44`), plugin moves and
//! copies (`0x1e01e8`), and the plugin sub-folder create, rename and delete (`0x248968` →
//! `0x76f2b8`, `0x248548` → `0x76aed8`, `0x77df68` → `0x76ec04`). A plugin preset is an
//! ordinary `.pb` with its `.json` sidecar in a plugin folder (`LocalPluginPresetFolder`,
//! vtable `0x122e058`); plugin presets are not in the search index (`0x216370` calls
//! `0x21ee8c` with the index flag clear).

use std::path::Path;

use tetrad_preset::PresetDocument;
use tetrad_storage::media::{SETLIST_SLOTS, Staged};

use super::{
    Committed, ItemRef, PLUGIN_DEPTH, PRESET_LISTS, StockLibrary, hashed, remove_preset_files,
    stored_name,
};
use crate::sidecar::Sidecar;
use crate::{Error, Result};

/// A plugin preset that [`StockLibrary::store_plugin_preset`] wrote.
#[derive(Debug)]
pub struct StoredPluginPreset {
    /// Device path of the stored `.pb`.
    pub path: String,
    /// The key it replaced (`vt+0x50`, `0x21abb8`); `None`: added (`vt+0x54`, `0x216370`).
    pub replaced: Option<String>,
    /// A rename stored the new file, but moving the favorites and recents or removing the old
    /// files failed (review r30 C2): the store is done, the old file may stay.
    pub tail_error: Option<Error>,
}

/// A plugin sub-folder that [`StockLibrary::rename_plugin_folder`] or
/// [`StockLibrary::delete_plugin_folder`] changed.
#[derive(Debug)]
pub struct PluginFolderChange {
    /// The folder key after the change (a rename: the new key).
    pub key: String,
    /// The folder changed on disk, but pointing the favorites and recents at it failed: the
    /// change is done, those lists may name the old paths.
    pub tail_error: Option<Error>,
}

impl StockLibrary {
    /// The writable directory behind a plugin folder key: `/media/p4/Plugins/<plugin>[/…]`, or
    /// the factory `/opt/neuraldsp/Plugins/<plugin>/User[/…]`, which stock links to the user
    /// plugin root of the same name. Other factory folders are read-only (`0x7054f0`). At most
    /// [`PLUGIN_DEPTH`] folders below the plugin; the directory must exist (O: no folder is
    /// created here).
    fn plugin_directory(&self, folder: &str) -> Result<String> {
        let user = self.plugin_user_path(folder)?;
        let directory = self.media.device_path(&format!("Plugins/{user}"))?;
        if !self.media.resolve(&directory)?.is_dir() {
            return Err(Error::Unsupported("a plugin folder without a directory"));
        }
        Ok(directory)
    }

    /// The path below the user `Plugins` root of a writable plugin folder key
    /// (`<plugin>[/…]`), as [`Self::plugin_directory`] maps it.
    fn plugin_user_path(&self, folder: &str) -> Result<String> {
        let below = |root: &tetrad_storage::media::MediaRoot| {
            folder
                .strip_prefix(root.device())
                .and_then(|rest| rest.strip_prefix("/Plugins/"))
        };
        let user = match (below(&self.media), self.factory.as_ref().and_then(below)) {
            (Some(rest), _) => rest.to_owned(),
            (None, Some(rest)) => match rest.split_once('/') {
                Some((plugin, tail)) if tail == "User" || tail.starts_with("User/") => {
                    format!("{plugin}{}", &tail["User".len()..])
                }
                _ => return Err(tetrad_storage::Error::ReadOnly.into()),
            },
            (None, None) => return Err(tetrad_storage::Error::DevicePath(folder.to_owned()).into()),
        };
        if user.split('/').count() > 1 + PLUGIN_DEPTH {
            return Err(Error::Unsupported("a plugin folder below the stock depth"));
        }
        Ok(user)
    }

    /// The directory of a writable plugin sub-folder. O: the plugin root and the factory
    /// `User` link (the user plugin root) do not change: stock would rename or remove the
    /// whole plugin.
    fn plugin_sub_folder(&self, folder: &str) -> Result<String> {
        if !self.plugin_user_path(folder)?.contains('/') {
            return Err(Error::Unsupported("the plugin root folder cannot change"));
        }
        self.plugin_directory(folder)
    }

    /// Cortex Control plugin sub-folder CREATE (`0x248968` → `addPluginSubfolder` `0x76f2b8`,
    /// C): `<parent>/<name>` made with `mkdir`; a path that exists is refused (stock logs "path
    /// already exists"). O: the parent is writable (`Self::plugin_directory`), the name is one
    /// plain path part, and the new folder is at most [`PLUGIN_DEPTH`] folders below the plugin
    /// (stock checks neither). Returns the new folder key, below the `parent` key.
    pub fn create_plugin_folder(&mut self, parent: &str, name: &str) -> Result<String> {
        let name = folder_name(name)?;
        if self.plugin_user_path(parent)?.split('/').count() > PLUGIN_DEPTH {
            return Err(Error::Unsupported("a plugin folder below the stock depth"));
        }
        let directory = self.plugin_directory(parent)?;
        let host = self.media.resolve(&format!("{directory}/{name}"))?;
        std::fs::create_dir(&host)?;
        Ok(format!("{parent}/{name}"))
    }

    /// Cortex Control plugin folder rename (`0x248548` → `0x76aed8` → `0x217858`, C): the same
    /// name does nothing; a read-only folder is not renamed; else the directory is renamed
    /// (`QDir::rename`, a failure only logs) and the folder's preset keys take the new path,
    /// with the favorites and recents (`0x7c537c`). Tetrad: every preset below the folder and
    /// every list entry of the folder or a folder below it follow (O: stock re-keys the
    /// folder's own presets). A name that another entry of the parent holds is refused.
    pub fn rename_plugin_folder(&mut self, folder: &str, name: &str) -> Result<PluginFolderChange> {
        let name = folder_name(name)?;
        let directory = self.plugin_sub_folder(folder)?;
        let (Some((parent_key, old)), Some((parent_directory, _))) =
            (folder.rsplit_once('/'), directory.rsplit_once('/'))
        else {
            return Err(tetrad_storage::Error::DevicePath(folder.to_owned()).into());
        };
        let key = format!("{parent_key}/{name}");
        if old == name {
            return Ok(PluginFolderChange {
                key,
                tail_error: None,
            });
        }
        let target = format!("{parent_directory}/{name}");
        let host = self.media.resolve(&target)?;
        if host.symlink_metadata().is_ok() {
            return Err(Error::Unsupported("a plugin folder of that name exists"));
        }
        std::fs::rename(self.media.resolve(&directory)?, &host)?;
        let tail_error = self.rekey_items(&directory, &target, folder, &key).err();
        Ok(PluginFolderChange { key, tail_error })
    }

    /// Cortex Control plugin folder DELETE (`0x250090` → `0x77df68(.., false)` → `0x76ec04`,
    /// C): a read-only folder stays; else the library forgets the folder and its children
    /// (`0x76a400`) and the directory goes with every file and folder in it (`0x699874`; a
    /// failure only logs). Tetrad removes the directory first, then drops the favorites and
    /// recents of its presets and of the folder.
    pub fn delete_plugin_folder(&mut self, folder: &str) -> Result<PluginFolderChange> {
        let directory = self.plugin_sub_folder(folder)?;
        let host = self.media.resolve(&directory)?;
        std::fs::remove_dir_all(&host)?;
        let tail_error = self.forget_folder_items(&directory, folder).err();
        Ok(PluginFolderChange {
            key: folder.to_owned(),
            tail_error,
        })
    }

    /// Points the favorites and recents below the directory `old` (presets) or the folder key
    /// `old_key` (their folder) at `new` and `new_key`.
    fn rekey_items(&mut self, old: &str, new: &str, old_key: &str, new_key: &str) -> Result<()> {
        for list in PRESET_LISTS {
            let entries = self.items(list)?;
            let moved: Vec<_> = (entries.iter())
                .map(|entry| ItemRef {
                    folder: below(&entry.folder, old_key)
                        .map_or_else(|| entry.folder.clone(), |rest| format!("{new_key}{rest}")),
                    item: below(&entry.item, old)
                        .map_or_else(|| entry.item.clone(), |rest| format!("{new}{rest}")),
                })
                .collect();
            if moved != entries {
                self.write_items(list, &moved)?;
            }
        }
        Ok(())
    }

    /// Drops the favorites and recents of a preset below the directory `directory` or of the
    /// folder key `key` and the folders below it.
    fn forget_folder_items(&mut self, directory: &str, key: &str) -> Result<()> {
        for list in PRESET_LISTS {
            let entries = self.items(list)?;
            let kept: Vec<_> = (entries.iter())
                .filter(|entry| {
                    below(&entry.item, directory).is_none() && below(&entry.folder, key).is_none()
                })
                .cloned()
                .collect();
            if kept != entries {
                self.write_items(list, &kept)?;
            }
        }
        Ok(())
    }

    /// A `.pb` below a user or factory `Plugins` directory.
    fn plugin_preset(&self, path: &str) -> bool {
        let below = |root: &tetrad_storage::media::MediaRoot| {
            path.strip_prefix(root.device())
                .is_some_and(|rest| rest.starts_with("/Plugins/"))
        };
        path.ends_with(".pb") && (below(&self.media) || self.factory.as_ref().is_some_and(below))
    }

    /// Stores a plugin preset in the plugin folder `folder` (the Cortex Control plugin import
    /// `0x24db44`, and the add of a plugin move). `key` names the item: when it is a preset of
    /// that folder, the preset is replaced (`vt+0x50`, `0x21abb8`), else added (`vt+0x54` →
    /// `0x216370` → `0x21ee8c`). The file is `<folder>/<name>.pb`, the name the sidecar `name`
    /// (else the preset's); the hash is the id of the stored path.
    ///
    /// Add: a free name (`0x699f18`). Replace: in place when the name keeps the file (both
    /// files staged before either is renamed); a new name writes a new file, points the
    /// favorites and recents of the old file at it (review r30 C5, O: the stock favorites step
    /// of `0x21abb8` is not traced), then removes the old one. Stock removes the old files
    /// first and writes `<name>.pb` over any file of that name; Tetrad writes first and takes a
    /// free name, so no other preset is overwritten (O).
    pub fn store_plugin_preset(
        &mut self,
        folder: &str,
        key: Option<&str>,
        document: &PresetDocument,
        sidecar: &Sidecar,
    ) -> Result<StoredPluginPreset> {
        let directory = self.plugin_directory(folder)?;
        let name = stored_name(document, Some(sidecar))
            .ok_or(Error::Unsupported("a plugin preset without a name"))?;
        let old = key.filter(|key| {
            key.rsplit_once('/')
                .is_some_and(|(parent, file)| parent == directory && file.ends_with(".pb"))
                && self.media.resolve(key).is_ok_and(|host| host.is_file())
        });
        if let Some(old) = old.filter(|old| *old == format!("{directory}/{name}.pb")) {
            let file = self.media.resolve(old)?;
            let staged = Staged::write(&file, &hashed(document, old)?.to_bytes())?;
            Staged::write(&file.with_extension("json"), &sidecar.encode())?.commit()?;
            staged.commit()?;
            return Ok(StoredPluginPreset {
                path: old.to_owned(),
                replaced: Some(old.to_owned()),
                tail_error: None,
            });
        }
        let (path, _) = self
            .stage_named(&directory, &name, document, sidecar)?
            .commit()?;
        let tail_error = old.and_then(|old| {
            let tail = self.follow_items(&[(old, &path, None)]);
            tail.and_then(|()| remove_preset_files(&self.media.resolve(old)?))
                .err()
        });
        Ok(StoredPluginPreset {
            path,
            replaced: old.map(str::to_owned),
            tail_error,
        })
    }

    /// Copies the plugin preset at `source` (user or factory) into slot `slot` of `setlist`, or
    /// into its first empty slot (`0x1e01e8` → `0x1de8ec` → `0x1de6c4`). An occupied slot is
    /// overwritten: stock removes the occupant first (`vt+0x4c`); Tetrad writes the copy, then
    /// points the slot at it, then drops the old preset as [`Self::delete`] does when no other
    /// slot names it. The copy is a free `<name>.pb` with the hash of its path (`0x21ee8c`).
    /// The source stays: stock removes it only for a plugin target. Not built: the account
    /// author stamp on a copy from a read-only folder (`0x765ac0` → `0x1d6ec8`). Returns the
    /// slot and the new path; after the setlist write the copy is done, and a failed drop of
    /// the old preset is [`Committed::tail_error`] (review r30 C2).
    pub fn copy_plugin_preset(
        &mut self,
        source: &str,
        setlist: &str,
        slot: Option<usize>,
    ) -> Result<Committed<(usize, String)>> {
        if !self.plugin_preset(source) {
            return Err(tetrad_storage::Error::DevicePath(source.to_owned()).into());
        }
        let mut slots = self.slots(setlist)?;
        let slot = match slot {
            Some(slot) if slot >= SETLIST_SLOTS => return Err(Error::SlotRange(slot)),
            Some(slot) => slot,
            None => (slots.iter().position(Option::is_none))
                .ok_or(Error::Unsupported("a setlist without an empty slot"))?,
        };
        let preset = self.load(source)?;
        let (path, _) = self
            .stage_new(&self.setlist_path(setlist)?, &preset)?
            .commit()?;
        let old = slots[slot].replace(path.clone());
        self.write_slots(setlist, &slots)?;
        let gone = old.filter(|old| !self.referenced(old));
        let tail_error = (gone.as_deref())
            .map(|old| {
                self.forget_items(old)?;
                remove_preset_files(&self.preset_file(old)?)
            })
            .and_then(Result::err);
        let removed: Vec<&str> = (gone.iter())
            .filter(|_| tail_error.is_none())
            .map(String::as_str)
            .collect();
        let search_index = self.update_search_index(&[&path], &removed);
        Ok(Committed {
            value: (slot, path),
            search_index,
            tail_error,
        })
    }

    /// Deletes a user plugin preset and its sidecar (`vt+0x4c`, the MOVE removal of
    /// `0x1e01e8`), and the favorites and recents that name it (I). Factory plugin presets are
    /// read-only.
    pub fn delete_plugin_preset(&mut self, path: &str) -> Result<()> {
        let (directory, _) = path
            .rsplit_once('/')
            .ok_or_else(|| tetrad_storage::Error::DevicePath(path.to_owned()))?;
        if !path.ends_with(".pb") || self.plugin_directory(directory)? != directory {
            return Err(tetrad_storage::Error::ReadOnly.into());
        }
        let file = self.media.resolve(path)?;
        if !file.is_file() {
            return Err(Error::PresetMissing(path.to_owned()));
        }
        remove_preset_files(&file)?;
        self.forget_items(path)
    }
}

/// A plugin sub-folder name (O): one plain path part.
fn folder_name(name: &str) -> Result<&str> {
    match name {
        "" | "." | ".." => Err(Error::Unsupported(
            "a plugin folder name that is no path part",
        )),
        _ if name.contains('/') || Path::new(name).components().count() != 1 => Err(
            Error::Unsupported("a plugin folder name that is no path part"),
        ),
        _ => Ok(name),
    }
}

/// The rest of `path` after `root` when `path` is `root` or below it (`""` or `/…`).
fn below<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    path.strip_prefix(root)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
}
