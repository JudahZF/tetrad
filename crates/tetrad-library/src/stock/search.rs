//! Search over the stock index files plus their recent-changes files.
//!
//! The library keeps `searchRecentChangesUser` current on every change, as
//! ZenUI does, and can rebuild `searchIndexUser` in the stock layout. The user
//! index is a cache: a missing or unreadable index means "rebuild", never a
//! failed change or a failed search.

use tetrad_storage::media::{MediaFile, MediaRoot, read_file, write_atomic};
use tetrad_storage::search_index::{
    BOOT_REBUILD_ABOVE_CHANGES, REBUILD_AFTER_CHANGES, SearchChanges, SearchEntry, SearchIndex,
    folder_key,
};

use super::{StaleSearchIndex, StockLibrary, remove_if_present};
use crate::Result;

/// The factory index files below `/opt/neuraldsp`.
const FACTORY_INDEX: &str = "searchIndexFactory";
const FACTORY_CHANGES: &str = "searchRecentChangesFactory";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Lowercase name as stored in the index.
    pub name: String,
    /// Device `.pb` path, or a capture id.
    pub path: String,
    pub factory: bool,
}

/// The lowercase index name of a preset path: its file name without `.pb`.
fn preset_name(path: &str) -> Option<String> {
    let file = path.rsplit('/').next()?;
    Some(file.strip_suffix(".pb")?.to_lowercase())
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// Adds the hits of one index: index entries not removed since, then added
/// presets by file name.
fn collect_hits(
    (index, changes): (&SearchIndex, &SearchChanges),
    query: &str,
    factory: bool,
    hits: &mut Vec<SearchHit>,
) {
    let removed = |path: &str| changes.removed.iter().any(|(_, p)| p == path);
    hits.extend(
        index
            .search(query)
            .into_iter()
            .filter(|entry| !removed(&entry.path))
            .map(|entry| SearchHit {
                name: entry.name.clone(),
                path: entry.path.clone(),
                factory,
            }),
    );
    let needle = query.to_lowercase();
    if needle.is_empty() {
        return;
    }
    hits.extend(changes.added.iter().filter_map(|(_, path)| {
        let name = preset_name(path)?;
        (name.contains(&needle) && !removed(path)).then(|| SearchHit {
            name,
            path: path.clone(),
            factory,
        })
    }));
}

impl StockLibrary {
    /// An index and its changes file; `None` when the index file is missing.
    fn load_index(
        &self,
        root: &MediaRoot,
        index: &str,
        changes: &str,
    ) -> Result<Option<(SearchIndex, SearchChanges)>> {
        let max = self.limits.max_list_entries;
        let Some(bytes) = read_file(&root.host().join(index), self.limits.max_file_bytes)? else {
            return Ok(None);
        };
        let index = SearchIndex::decode(&bytes, max)?;
        let changes = match read_file(&root.host().join(changes), self.limits.max_file_bytes)? {
            Some(bytes) => SearchChanges::decode(&bytes, max)?,
            None => SearchChanges::default(),
        };
        Ok(Some((index, changes)))
    }

    /// The user index, or `None` when it needs a rebuild: a file is missing
    /// or does not load (the stock boot rule of `0x0073545c`).
    fn user_index(&self) -> Option<(SearchIndex, SearchChanges)> {
        self.load_index(
            &self.media,
            MediaFile::SearchIndexUser.relative(),
            MediaFile::SearchRecentChangesUser.relative(),
        )
        .ok()
        .flatten()
    }

    fn factory_index(&self, factory: &MediaRoot) -> Result<Option<&(SearchIndex, SearchChanges)>> {
        if let Some(cached) = self.factory_index.get() {
            return Ok(cached.as_ref());
        }
        let loaded = self.load_index(factory, FACTORY_INDEX, FACTORY_CHANGES)?;
        Ok(self.factory_index.get_or_init(|| loaded).as_ref())
    }

    /// Index entries of every preset slot, in setlist and slot order. A
    /// setlist that cannot be read is skipped.
    fn preset_entries(&self) -> Result<Vec<SearchEntry>> {
        let mut entries = Vec::new();
        for setlist in self.setlists()? {
            let Ok(slots) = self.slots(&setlist.name) else {
                continue;
            };
            let key = folder_key(&setlist.path);
            entries.extend(slots.into_iter().flatten().filter_map(|path| {
                Some(SearchEntry {
                    name: preset_name(&path)?,
                    folder_key: key,
                    path,
                })
            }));
        }
        Ok(entries)
    }

    /// Case-insensitive substring search over user and factory entries.
    /// Removed entries are hidden; added presets match by file name. While
    /// the user index needs a rebuild, user presets are searched in the
    /// setlists directly.
    pub fn search(&self, query: &str) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();
        match self.user_index() {
            Some((index, changes)) => collect_hits((&index, &changes), query, false, &mut hits),
            None => {
                let index = SearchIndex::build(self.preset_entries()?)?;
                collect_hits((&index, &SearchChanges::default()), query, false, &mut hits);
            }
        }
        if let Some(factory) = &self.factory
            && let Some((index, changes)) = self.factory_index(factory)?
        {
            collect_hits((index, changes), query, true, &mut hits);
        }
        hits.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
        hits.dedup_by(|a, b| a.path == b.path);
        Ok(hits)
    }

    /// Records preset paths added and removed since the index was built. Like
    /// ZenUI, rebuilds the index once [`REBUILD_AFTER_CHANGES`] accumulate, or
    /// at once when the index needs a rebuild.
    fn record_search_changes(&mut self, added: &[&str], removed: &[&str]) -> Result<()> {
        let Some((_, mut changes)) = self.user_index() else {
            return self.rebuild_search_index();
        };
        for path in removed {
            changes.added.retain(|(_, p)| p != path);
            if !changes.removed.iter().any(|(_, p)| p == path) {
                changes
                    .removed
                    .push((folder_key(parent(path)), (*path).to_owned()));
            }
        }
        for path in added {
            changes.removed.retain(|(_, p)| p != path);
            if !changes.added.iter().any(|(_, p)| p == path) {
                changes
                    .added
                    .push((folder_key(parent(path)), (*path).to_owned()));
            }
        }
        self.crash_point("search: changes")?;
        write_atomic(
            &self.media.file(MediaFile::SearchRecentChangesUser),
            &changes.encode(),
        )?;
        if changes.added.len() + changes.removed.len() >= REBUILD_AFTER_CHANGES {
            self.rebuild_search_index()?;
        }
        Ok(())
    }

    /// The last step of a saved change. A failure drops both user index files
    /// (best effort), so the index is rebuilt later, and comes back as a
    /// warning instead of an error.
    pub(super) fn update_search_index(
        &mut self,
        added: &[&str],
        removed: &[&str],
    ) -> Option<StaleSearchIndex> {
        let error = self.record_search_changes(added, removed).err()?;
        for file in [
            MediaFile::SearchIndexUser,
            MediaFile::SearchRecentChangesUser,
        ] {
            // A file that cannot be removed leaves the old index; the
            // returned warning still reports the index as stale.
            let _ = remove_if_present(&self.media.file(file));
        }
        Some(StaleSearchIndex(error))
    }

    /// The boot check of `0x0073545c`: rebuilds the user index when it is
    /// missing or does not load, or when more than
    /// [`BOOT_REBUILD_ABOVE_CHANGES`] changes wait. Returns true when it
    /// rebuilt.
    pub fn prepare_search_index(&mut self) -> Result<bool> {
        let current = self.user_index().is_some_and(|(_, changes)| {
            changes.added.len() + changes.removed.len() <= BOOT_REBUILD_ABOVE_CHANGES
        });
        if !current {
            self.rebuild_search_index()?;
        }
        Ok(!current)
    }

    /// Rebuilds `searchIndexUser` in stock order: every setlist in
    /// `user_preset_folders` order and slot order, then the earlier non-preset
    /// entries (captures, whose names live in encrypted metadata) that were not
    /// removed. Only non-preset additions stay in `searchRecentChangesUser`.
    /// An unreadable old index adds no capture entries.
    pub fn rebuild_search_index(&mut self) -> Result<()> {
        let (old, mut changes) = self.user_index().unwrap_or_default();
        let mut entries = self.preset_entries()?;
        let presets = self.media.device_path(MediaFile::Presets.relative())?;
        entries.extend(old.entries.into_iter().filter(|entry| {
            !entry.path.starts_with(&presets)
                && !changes.removed.iter().any(|(_, p)| *p == entry.path)
        }));
        let index = SearchIndex::build(entries)?;
        write_atomic(
            &self.media.file(MediaFile::SearchIndexUser),
            &index.encode(),
        )?;
        // ZenUI clears all three lists; capture additions stay because their
        // names are only in device-encrypted metadata.
        changes.recent.clear();
        changes
            .added
            .retain(|(_, path)| !path.starts_with(&presets));
        changes.removed.clear();
        Ok(write_atomic(
            &self.media.file(MediaFile::SearchRecentChangesUser),
            &changes.encode(),
        )?)
    }
}
