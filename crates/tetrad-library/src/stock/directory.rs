//! Directory summaries and edits over stock files and SQLite rows.

use std::collections::BTreeSet;
use std::fs;

use tetrad_storage::media::{SETLIST_SLOTS, check_name, read_file};
use tetrad_storage::stock::{FolderType, NewFolder, ProductType, StockDb, StockFolder};

use super::{DEFAULT_SETLIST, ItemList, PRESET_LISTS, Saved, StockLibrary};
use crate::sidecar::Sidecar;
use crate::{Error, Result};

/// A slot's occupancy comes from `setlist`, never from its display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSummary {
    pub slot: usize,
    pub path: Option<String>,
    pub name: String,
    pub author: String,
    pub date: String,
}

/// One plugin. A factory plugin's `User` child is the user root of the same
/// name (stock links `/opt/neuraldsp/Plugins/<plugin>/User` to it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFolder {
    pub name: String,
    pub path: String,
    pub read_only: bool,
    pub children: Vec<String>,
    /// `.pb` presets at most [`PLUGIN_DEPTH`] folders below the plugin.
    pub presets: Vec<PluginPreset>,
    /// Device paths of folders that could not be read; they are left out (stock not read,
    /// inferred, review r11 N12).
    pub unreadable: Vec<String>,
}

/// A plugin preset: an ordinary `.pb` preset with its `.json` sidecar (preset case of
/// `0x7efc38`). A missing or unreadable sidecar leaves the stem and empty roles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginPreset {
    /// Folder below the plugin (`""`, `Neural DSP`, `Artists/<artist>`, `User`).
    pub folder: String,
    pub path: String,
    /// The sidecar `name`, else the file stem.
    pub name: String,
    pub author: String,
    pub date: String,
    /// The sidecar `instrument_type` text (`guitar`, `bass`, … ; `0x7ec714`).
    pub instrument: String,
}

/// `<plugin>/Artists/<artist>/<preset>.pb` is the deepest stock layout.
pub const PLUGIN_DEPTH: usize = 2;

/// How Move treats an occupied destination (`0x1e8290`: "You've dropped on an
/// existing preset", SWAP / OVERWRITE / CANCEL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveMode {
    /// "presets will change places with each other".
    Swap,
    /// "will delete previous and leave the one you just dragged over".
    Overwrite,
}

impl StockLibrary {
    /// Sidecars carry the sort roles (`0x7ee62c`, preset case). Missing sidecars
    /// retain the file name and leave unavailable roles empty.
    pub fn directory_slots(&self, setlist: &str) -> Result<Vec<SlotSummary>> {
        self.slots(setlist)?
            .into_iter()
            .enumerate()
            .map(|(slot, path)| {
                let sidecar = match &path {
                    Some(path) => read_file(
                        &self.preset_file_in(path, true)?.with_extension("json"),
                        self.limits.max_file_bytes,
                    )?
                    .map(|bytes| Sidecar::parse(&bytes))
                    .transpose()?,
                    None => None,
                };
                let field = |key| {
                    sidecar
                        .as_ref()
                        .and_then(|s| s.text(key))
                        .unwrap_or_default()
                        .to_owned()
                };
                let name = sidecar
                    .as_ref()
                    .and_then(|s| s.text("name"))
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        path.as_deref()
                            .map_or("Unsaved", |p| {
                                p.rsplit('/').next().unwrap_or(p).trim_end_matches(".pb")
                            })
                            .to_owned()
                    });
                Ok(SlotSummary {
                    slot,
                    name,
                    author: field("author"),
                    date: field("date"),
                    path,
                })
            })
            .collect()
    }

    /// Stock roots: `/media/p4/Plugins` and `/opt/neuraldsp/Plugins` (`0xb1d68`).
    /// A user plugin with a factory twin becomes its `User` child; other user
    /// plugins stay separate. A directory walk that reads each preset's sidecar
    /// (no payload).
    pub fn plugin_folders(&self) -> Result<Vec<PluginFolder>> {
        self.plugin_tree(true)
    }

    /// [`Self::plugin_folders`] without the factory plugin presets: a factory
    /// plugin keeps its children and its `User` twin's presets. The full walk
    /// reads one sidecar per factory preset (3 460 on CorOS 4.1.0); listings
    /// that leave the read-only plugin folders out need none of them.
    pub fn writable_plugin_folders(&self) -> Result<Vec<PluginFolder>> {
        self.plugin_tree(false)
    }

    fn plugin_tree(&self, factory_presets: bool) -> Result<Vec<PluginFolder>> {
        let max_bytes = self.limits.max_file_bytes;
        let mut user = plugin_roots(&self.media, false, max_bytes, true)?;
        let mut folders = match &self.factory {
            Some(factory) => plugin_roots(factory, true, max_bytes, factory_presets)?,
            None => Vec::new(),
        };
        for folder in &mut folders {
            let Some(at) = user.iter().position(|u| u.name == folder.name) else {
                continue;
            };
            let twin = user.remove(at);
            folder.children.retain(|child| child != "User");
            folder.children.push("User".into());
            folder
                .presets
                .extend(twin.presets.into_iter().map(|preset| PluginPreset {
                    folder: match preset.folder.as_str() {
                        "" => "User".into(),
                        rest => format!("User/{rest}"),
                    },
                    ..preset
                }));
        }
        folders.extend(user);
        for folder in &mut folders {
            folder.children.sort_by_cached_key(|name| child_order(name));
        }
        folders.sort_by_key(|folder| folder.name.to_lowercase());
        Ok(folders)
    }

    /// Delete the setlist and its known presets, then remove its directory.
    /// Stock recursively deletes (`0x76c170`). Preflight keeps unrelated files
    /// and files named by another setlist; no unrecognized file is removed.
    pub fn delete_setlist_with_presets(&mut self, name: &str) -> Result<Saved<()>> {
        if name == DEFAULT_SETLIST {
            return Err(Error::Unsupported("the default setlist cannot be deleted"));
        }
        let slots = self.slots(name)?;
        let path = self.setlist_path(name)?;
        let directory = self.media.resolve(&path)?;
        let mut known = BTreeSet::from(["setlist".to_owned()]);
        for file in slots.iter().flatten() {
            let host = self.preset_file(file)?;
            if host.parent() != Some(directory.as_path()) {
                return Err(Error::Unsupported("a setlist with foreign preset paths"));
            }
            let file = host
                .file_name()
                .ok_or(Error::Unsupported("preset file name"))?
                .to_string_lossy()
                .into_owned();
            known.insert(file.clone());
            known.insert(format!("{}.json", file.trim_end_matches(".pb")));
        }
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type()?.is_file()
                || (!known.contains(&file) && !tetrad_storage::media::is_temp_file(&file))
            {
                return Err(Error::SetlistHasFiles {
                    setlist: name.to_owned(),
                    files: vec![file],
                });
            }
        }
        for other in self.setlists()?.iter().filter(|s| s.name != name) {
            if self
                .slots(&other.name)?
                .iter()
                .flatten()
                .any(|p| slots.iter().flatten().any(|s| s == p))
            {
                return Err(Error::Unsupported("presets shared with another setlist"));
            }
        }
        let mut stale = None;
        for (slot, file) in slots.iter().enumerate() {
            if file.is_some() {
                stale = self.delete(name, slot)?.search_index.or(stale);
            }
        }
        self.delete_setlist(name)?;
        for list in PRESET_LISTS {
            let entries = self
                .items(list)?
                .into_iter()
                .filter(|entry| entry.folder != path)
                .collect::<Vec<_>>();
            self.write_items(list, &entries)?;
        }
        Ok(Saved {
            value: (),
            search_index: stale,
        })
    }

    /// Row Move inside one setlist. An empty destination is a plain move
    /// (`0x1e68d0`); an occupied one follows `mode` (`0x1e8290`). Overwrite
    /// deletes the destination preset as [`Self::delete`] does. Files are
    /// never renamed.
    pub fn move_slot(
        &mut self,
        setlist: &str,
        from: usize,
        to: usize,
        mode: MoveMode,
    ) -> Result<Saved<()>> {
        if from >= SETLIST_SLOTS {
            return Err(Error::SlotRange(from));
        }
        if to >= SETLIST_SLOTS {
            return Err(Error::SlotRange(to));
        }
        let slots = self.slots(setlist)?;
        if slots[from].is_none() {
            return Err(Error::SlotEmpty(from));
        }
        let mut search_index = None;
        if from != to {
            if mode == MoveMode::Overwrite && slots[to].is_some() {
                search_index = self.delete(setlist, to)?.search_index;
            }
            self.swap_slots(setlist, from, to)?;
        }
        Ok(Saved {
            value: (),
            search_index,
        })
    }

    /// Remove every folder reference to a deleted media id. A product can
    /// belong to several folders; deleting it removes all favorite/recent links.
    pub fn forget_media(
        &mut self,
        collection: tetrad_storage::media::Collection,
        id: &str,
    ) -> Result<()> {
        for list in [
            ItemList::Favorites(collection),
            ItemList::Recents(collection),
        ] {
            let entries = self
                .items(list)?
                .into_iter()
                .filter(|entry| entry.item != id)
                .collect::<Vec<_>>();
            self.write_items(list, &entries)?;
        }
        Ok(())
    }
}

/// The plugin directories of one root. Symlinks are not followed (the factory
/// `User` link is replaced by the user root in [`StockLibrary::plugin_folders`]).
/// The plugins below `root`; `presets` false leaves every preset list empty.
fn plugin_roots(
    root: &tetrad_storage::media::MediaRoot,
    read_only: bool,
    max_bytes: u64,
    presets: bool,
) -> Result<Vec<PluginFolder>> {
    let host = root.resolve(&root.device_path("Plugins")?)?;
    let mut folders = Vec::new();
    for (name, _) in named_entries(&host, true)? {
        let path = root.device_path(&format!("Plugins/{name}"))?;
        let mut walk = Walk::default();
        if presets {
            walk.presets(&host.join(&name), &path, "", PLUGIN_DEPTH, max_bytes);
        }
        let Walk {
            mut presets,
            unreadable,
        } = walk;
        presets.sort_by_cached_key(|preset| {
            (preset.folder.to_lowercase(), preset.name.to_lowercase())
        });
        folders.push(PluginFolder {
            // An unreadable plugin folder is already in `unreadable`.
            children: named_entries(&host.join(&name), true)
                .unwrap_or_default()
                .into_iter()
                .map(|(child, _)| child)
                .collect(),
            name,
            path,
            read_only,
            presets,
            unreadable,
        });
    }
    Ok(folders)
}

/// The presets below one plugin, and the folders that could not be read.
#[derive(Default)]
struct Walk {
    presets: Vec<PluginPreset>,
    unreadable: Vec<String>,
}

impl Walk {
    fn presets(
        &mut self,
        host: &std::path::Path,
        device: &str,
        folder: &str,
        depth: usize,
        max_bytes: u64,
    ) {
        let Ok(entries) = named_entries(host, false) else {
            self.unreadable.push(device.to_owned());
            return;
        };
        for (name, is_dir) in entries {
            if is_dir {
                if depth > 0 {
                    let child = if folder.is_empty() {
                        name.clone()
                    } else {
                        format!("{folder}/{name}")
                    };
                    self.presets(
                        &host.join(&name),
                        &format!("{device}/{name}"),
                        &child,
                        depth - 1,
                        max_bytes,
                    );
                }
            } else if let Some(stem) = name.strip_suffix(".pb")
                && !stem.starts_with("__")
            {
                let sidecar = read_file(&host.join(format!("{stem}.json")), max_bytes)
                    .ok()
                    .flatten()
                    .and_then(|bytes| Sidecar::parse(&bytes).ok())
                    .unwrap_or_default();
                let text = |key| sidecar.text(key).unwrap_or_default().to_owned();
                self.presets.push(PluginPreset {
                    folder: folder.to_owned(),
                    path: format!("{device}/{name}"),
                    name: sidecar
                        .text("name")
                        .filter(|name| !name.is_empty())
                        .unwrap_or(stem)
                        .to_owned(),
                    author: text("author"),
                    date: text("date"),
                    instrument: text("instrument_type"),
                });
            }
        }
    }
}

/// Valid-named directories (and files unless `dirs_only`) of `host`; a missing
/// directory is empty. Symlinks are skipped.
pub(super) fn named_entries(
    host: &std::path::Path,
    dirs_only: bool,
) -> Result<Vec<(String, bool)>> {
    let entries = match fs::read_dir(host) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut named = Vec::new();
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if !(kind.is_dir() || (!dirs_only && kind.is_file())) {
            continue;
        }
        if let Some(name) = entry.file_name().to_str()
            && check_name(name).is_ok()
        {
            named.push((name.to_owned(), kind.is_dir()));
        }
    }
    Ok(named)
}

/// Stock sidebar order in a plugin (shot of the nested view): the main
/// artist folder, then Artists, Neural DSP and User.
fn child_order(name: &str) -> (u8, String) {
    let rank = match name {
        "Artists" => 1,
        "Neural DSP" => 2,
        "User" => 3,
        _ => 0,
    };
    (rank, name.to_lowercase())
}

fn editable_folder(folders: &[StockFolder], id: i64) -> tetrad_storage::Result<&StockFolder> {
    let folder = folders
        .iter()
        .find(|folder| folder.id == id)
        .ok_or(tetrad_storage::Error::FolderMissing(id))?;
    if folder.read_only || folder.folder_type != FolderType::User {
        return Err(tetrad_storage::Error::ReadOnly);
    }
    Ok(folder)
}

/// Writable folders of one media type, the user root included (stock
/// `0x00766790` refuses a new folder at 30: "30 folders" in the manual).
pub const MAX_MEDIA_FOLDERS: usize = 30;
/// A new folder's parent must have fewer ancestors than this (`0x00766790`:
/// `depth < 4`; the root has none): "nested up to four levels deep".
pub const MAX_MEDIA_FOLDER_DEPTH: usize = 4;
/// Byte cap of a generated duplicate name (`0x000ebe90`).
const MAX_GENERATED_NAME_BYTES: usize = 16;

fn writable_folders(folders: &[StockFolder], kind: ProductType) -> usize {
    folders
        .iter()
        .filter(|folder| folder.product_type == kind && !folder.read_only)
        .count()
}

/// Ancestors of a folder (`0x00705874`).
fn folder_depth(folders: &[StockFolder], id: i64) -> tetrad_storage::Result<usize> {
    let mut depth = 0;
    let mut current = id;
    loop {
        let folder = folders
            .iter()
            .find(|folder| folder.id == current)
            .ok_or(tetrad_storage::Error::FolderMissing(current))?;
        if folder.parent == 0 {
            return Ok(depth);
        }
        depth += 1;
        if depth > folders.len() {
            return Err(tetrad_storage::Error::Cycle);
        }
        current = folder.parent;
    }
}

/// Stock creates a user folder linked to its parent (`0x76d584`) when
/// `0x00766790` allows it: fewer than [`MAX_MEDIA_FOLDERS`] writable folders
/// of the type and a parent depth below [`MAX_MEDIA_FOLDER_DEPTH`]. Refusing a
/// sibling with the same name is owned.
pub fn create_media_folder(
    db: &mut StockDb,
    kind: ProductType,
    parent: i64,
    name: &str,
) -> Result<i64> {
    check_name(name)?;
    if !matches!(kind, ProductType::Ir | ProductType::Capture) {
        return Err(Error::Unsupported("media folder product type"));
    }
    Ok(db.transaction(|tx| {
        let folders = tx.folders()?;
        let parent_folder = folders
            .iter()
            .find(|f| f.id == parent)
            .ok_or(tetrad_storage::Error::FolderMissing(parent))?;
        if parent_folder.product_type != kind || parent_folder.read_only {
            return Err(tetrad_storage::Error::ReadOnly);
        }
        if folders
            .iter()
            .any(|f| f.parent == parent && f.product_type == kind && f.name == name)
        {
            return Err(tetrad_storage::Error::Format("folder name already exists"));
        }
        if writable_folders(&folders, kind) >= MAX_MEDIA_FOLDERS
            || folder_depth(&folders, parent)? >= MAX_MEDIA_FOLDER_DEPTH
        {
            return Err(tetrad_storage::Error::Limit);
        }
        tx.create_folder(&NewFolder {
            product_type: kind,
            folder_type: FolderType::User,
            name,
            parent,
            read_only: false,
        })
    })?)
}

/// "Add to folder": links existing products of the folder's type to a
/// writable user folder or root, in one transaction (`folder_content`,
/// `0x012045c8`; a folder holds links, not copies).
pub fn add_to_media_folder(db: &mut StockDb, folder: i64, ids: &[String]) -> Result<()> {
    Ok(db.transaction(|tx| {
        let target = tx
            .folders()?
            .into_iter()
            .find(|f| f.id == folder)
            .ok_or(tetrad_storage::Error::FolderMissing(folder))?;
        for id in ids {
            let product = tx
                .product(id)?
                .ok_or_else(|| tetrad_storage::Error::ProductMissing(id.clone()))?;
            if product.product_type != target.product_type {
                return Err(tetrad_storage::Error::ReadOnly);
            }
            tx.link(folder, id)?;
        }
        Ok(())
    })?)
}

/// DUPLICATE (`0x0076dabc`): copies a user folder and its subfolders with
/// their links under the same parent. The copy takes the next free name among
/// the writable siblings ([`next_folder_name`], `0x00769e98`); subfolders
/// keep their names (`0x0076d7c4`). `0x0076692c` allows it when the
/// folder is not the root and its writable subtree plus the writable folders
/// of the type stay below 31. Copying the links is inferred from the manual
/// ("a copy of the selected folder and its content").
pub fn duplicate_media_folder(db: &mut StockDb, id: i64) -> Result<i64> {
    Ok(db.transaction(|tx| {
        let folders = tx.folders()?;
        let source = editable_folder(&folders, id)?;
        let mut subtree = vec![id];
        let mut next = 0;
        while let Some(&current) = subtree.get(next) {
            next += 1;
            for child in folders
                .iter()
                .filter(|f| f.parent == current && !f.read_only)
            {
                if subtree.contains(&child.id) {
                    return Err(tetrad_storage::Error::Cycle);
                }
                subtree.push(child.id);
            }
        }
        if subtree.len() + writable_folders(&folders, source.product_type) > MAX_MEDIA_FOLDERS {
            return Err(tetrad_storage::Error::Limit);
        }
        let siblings: Vec<&str> = folders
            .iter()
            .filter(|f| f.parent == source.parent && !f.read_only)
            .map(|f| f.name.as_str())
            .collect();
        let mut name = source.name.clone();
        while siblings.contains(&name.as_str()) {
            name = next_folder_name(&name);
        }
        let mut copies = vec![(id, source.parent, name)];
        let mut top = None;
        while let Some((from, parent, name)) = copies.pop() {
            let copy = tx.create_folder(&NewFolder {
                product_type: source.product_type,
                folder_type: FolderType::User,
                name: &name,
                parent,
                read_only: false,
            })?;
            top.get_or_insert(copy);
            for product in tx.folder_products(from)? {
                tx.link(copy, &product)?;
            }
            copies.extend(
                folders
                    .iter()
                    .filter(|f| f.parent == from && !f.read_only)
                    .map(|f| (f.id, copy, f.name.clone())),
            );
        }
        top.ok_or(tetrad_storage::Error::FolderMissing(id))
    })?)
}

/// The stock name increment (`0x000ebe90`): split on spaces; a last word
/// that starts with an integer (`strtol`) becomes that integer plus one,
/// otherwise " 2" is appended. When the result exceeds 16 bytes, the base is
/// cut to leave 16 bytes in total. Owned: the cut never splits a character,
/// and an integer out of range counts as no integer (stock throws).
pub fn next_folder_name(name: &str) -> String {
    let mut words: Vec<&str> = name.split(' ').collect();
    if words.len() > 1 && words.last() == Some(&"") {
        words.pop();
    }
    let last = words.last().copied().unwrap_or_default();
    let digits = last.trim_start();
    let sign = usize::from(digits.starts_with(['+', '-']));
    let end = sign
        + digits[sign..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
    let number = (end > sign)
        .then(|| digits[..end].parse::<i64>().ok())
        .flatten()
        .and_then(|n| n.checked_add(1));
    let (mut base, number) = match number {
        Some(number) => (
            words[..words.len() - 1]
                .iter()
                .map(|word| format!("{word} "))
                .collect::<String>(),
            number.to_string(),
        ),
        None => (format!("{name} "), "2".to_owned()),
    };
    if base.len() + number.len() > MAX_GENERATED_NAME_BYTES {
        let mut cut = (MAX_GENERATED_NAME_BYTES - 1).saturating_sub(number.len());
        while !base.is_char_boundary(cut) {
            cut -= 1;
        }
        base.truncate(cut);
        base.push(' ');
    }
    base + &number
}

pub fn rename_media_folder(db: &mut StockDb, id: i64, name: &str) -> Result<()> {
    check_name(name)?;
    Ok(db.transaction(|tx| {
        let folders = tx.folders()?;
        let target = editable_folder(&folders, id)?;
        if folders.iter().any(|f| {
            f.id != id
                && f.parent == target.parent
                && f.product_type == target.product_type
                && f.name == name
        }) {
            return Err(tetrad_storage::Error::Format("folder name already exists"));
        }
        tx.rename_folder(id, name)
    })?)
}

/// Delete folder links, optionally products, in one transaction. Children
/// are deleted first. The user root and read-only folders stay protected.
pub fn delete_media_folder(
    db: &mut StockDb,
    id: i64,
    remove_products: bool,
) -> Result<Vec<String>> {
    Ok(db.transaction(|tx| {
        let folders = tx.folders()?;
        editable_folder(&folders, id)?;
        let mut pending = vec![id];
        let mut tree = Vec::new();
        while let Some(id) = pending.pop() {
            if tree.contains(&id) {
                return Err(tetrad_storage::Error::Format("folder cycle"));
            }
            editable_folder(&folders, id)?;
            tree.push(id);
            pending.extend(folders.iter().filter(|f| f.parent == id).map(|f| f.id));
        }
        let mut removed = BTreeSet::new();
        if remove_products {
            for id in &tree {
                removed.extend(tx.folder_products(*id)?);
            }
            for product in &removed {
                tx.delete_product(product)?;
            }
        }
        for id in tree.into_iter().rev() {
            tx.delete_folder(id)?;
        }
        Ok(removed.into_iter().collect())
    })?)
}
