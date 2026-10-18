//! The user capture steps of the stock capture manager (`0x704198`, boot) and of the restore reload
//! (`0x3fcf64`): the legacy capture file translation (`0x703900`) and the corruption check
//! (`0x702f70`); the legacy format conversion (`0x702d58`) is in `conversion.rs`. Their marker
//! files live outside the media root; the caller keeps them.

use std::fs;
use std::path::PathBuf;

use tetrad_media::factory::DeviceCipher;
use tetrad_storage::media::{LEGACY_TEMP_MARKER, read_file, write_atomic};
use tetrad_storage::stock::{CaptureStore, DownloadKind, ProductType, StockDb};

use super::StockLibrary;
use super::conversion::CONVERTED_SUFFIX;
use super::media::{LEGACY_CAPTURE_DIR, MEDIA_LIBRARY_CAP, legacy_files};
use crate::Result;

/// The suffix of a legacy capture file's re-keyed copy ([`StockLibrary::stage_legacy_capture`]).
/// It exists only between a cut run and the next boot; it is never a stock file.
pub const REKEYED_SUFFIX: &str = ".tetrad-local";

/// The staged-copy suffixes as zen-rs (the earlier name of Tetrad) wrote them, with their
/// Tetrad names ([`StockLibrary::migrate_zen_rs_files`]).
const ZEN_RS_SUFFIXES: [(&str, &str); 2] = [
    (".zen-local", REKEYED_SUFFIX),
    (".zen-converted", CONVERTED_SUFFIX),
];

fn legacy_capture_files(name: &str) -> [String; 2] {
    [format!("{name}.cns"), format!("{name}.json")]
}

impl StockLibrary {
    /// The legacy capture names of `0x703900`: each `*.cns` file name without `.cns`
    /// (`QString::chop(4)`), sorted. Empty when the directory does not exist.
    pub fn legacy_capture_names(&self) -> Result<Vec<String>> {
        let Some(dir) = self.legacy_capture_dir()? else {
            return Ok(Vec::new());
        };
        Ok(legacy_files(&dir, ".cns")?
            .into_iter()
            .map(|file| file.trim_end_matches(".cns").to_owned())
            .collect())
    }

    /// `0x703900` for one capture, first half: `<name>.cns`, then `<name>.json`, each re-keyed
    /// as a whole from `global` (seed only) to `local` (seed and serial) through `0x6b03d4`.
    /// A missing or empty file is left (stock writes only a translated buffer). O (review r23
    /// K4): the re-keyed bytes go to a complete [`REKEYED_SUFFIX`] copy and the file stays as
    /// it is, so staging again after a cut re-keys the same file once more, not twice. The
    /// caller journals the capture, then [`Self::commit_legacy_capture`]. Stock re-keys in
    /// place, and a cut between a write and its journal line undoes the re-key.
    pub fn stage_legacy_capture(
        &self,
        name: &str,
        global: &DeviceCipher,
        local: &DeviceCipher,
    ) -> Result<()> {
        let Some(dir) = self.legacy_capture_dir()? else {
            return Ok(());
        };
        for file in legacy_capture_files(name) {
            let path = dir.join(&file);
            let Some(mut bytes) = read_file(&path, self.limits.max_file_bytes)? else {
                continue;
            };
            if bytes.is_empty() {
                continue;
            }
            global.apply(&mut bytes);
            local.apply(&mut bytes);
            write_atomic(&dir.join(format!("{file}{REKEYED_SUFFIX}")), &bytes)?;
        }
        Ok(())
    }

    /// The second half of [`Self::stage_legacy_capture`], after the capture's journal line is
    /// on disk: each staged copy is renamed over its file. A capture without staged copies
    /// (committed, or nothing to re-key) is left, so a journaled capture can be committed again
    /// after a cut.
    pub fn commit_legacy_capture(&self, name: &str) -> Result<()> {
        let Some(dir) = self.legacy_capture_dir()? else {
            return Ok(());
        };
        for file in legacy_capture_files(name) {
            let staged = dir.join(format!("{file}{REKEYED_SUFFIX}"));
            if fs::symlink_metadata(&staged).is_ok_and(|metadata| metadata.is_file()) {
                fs::rename(&staged, dir.join(&file))?;
                fs::File::open(&dir)?.sync_all()?;
            }
        }
        Ok(())
    }

    /// The zen-rs to Tetrad rename in the legacy capture directory, the only place that holds
    /// staged copies (O). Run it before the journaled captures commit. Each `*.zen-local` and
    /// `*.zen-converted` file gets its Tetrad suffix ([`REKEYED_SUFFIX`], [`CONVERTED_SUFFIX`])
    /// when no file has that name yet. When both exist, the Tetrad file is kept as the newer
    /// one, the old file stays, and its name is returned for the log. A temporary file of
    /// zen-rs ([`LEGACY_TEMP_MARKER`]) is a crash leftover and is removed. Stock files never
    /// have these names; a second run changes nothing.
    pub fn migrate_zen_rs_files(&self) -> Result<Vec<String>> {
        let Some(dir) = self.legacy_capture_dir()? else {
            return Ok(Vec::new());
        };
        let (mut kept, mut changed) = (Vec::new(), false);
        for file in legacy_files(&dir, "")? {
            let path = dir.join(&file);
            if file.contains(LEGACY_TEMP_MARKER) {
                fs::remove_file(&path)?;
                changed = true;
                continue;
            }
            let renamed = ZEN_RS_SUFFIXES
                .iter()
                .find_map(|(old, new)| file.strip_suffix(old).map(|stem| format!("{stem}{new}")));
            let Some(renamed) = renamed else {
                continue;
            };
            let target = dir.join(renamed);
            if fs::symlink_metadata(&target).is_ok() {
                kept.push(file);
            } else {
                fs::rename(&path, &target)?;
                changed = true;
            }
        }
        if changed {
            fs::File::open(&dir)?.sync_all()?;
        }
        Ok(kept)
    }

    pub(super) fn legacy_capture_dir(&self) -> Result<Option<PathBuf>> {
        let dir = self.media.resolve(LEGACY_CAPTURE_DIR)?;
        Ok(fs::symlink_metadata(&dir)
            .is_ok_and(|metadata| metadata.is_dir())
            .then_some(dir))
    }
}

/// The verdict on one stored capture, as the corruption check reads it: metadata through
/// `0x7efc38`, then rawdata through `0x7ed2c8`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureCheck {
    Sound,
    /// "… capture metadata corrupt, deleting": stock lists the row's id.
    BadMetadata(String),
    /// "… capture corrupt, deleting": stock lists the capture's name (I: `local_a8`).
    BadRawdata {
        name: String,
        reason: String,
    },
    /// Tetrad cannot read the row now (for example no NC2 key): kept (O).
    Unchecked(String),
}

/// A capture the check took out of the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorruptCapture {
    pub store: CaptureStore,
    pub id: String,
    /// The text for the "Corrupt Neural Capture" dialog list.
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorruptionCheck {
    pub removed: Vec<CorruptCapture>,
    /// Rows that failed but stay in the library: too many failed for one check
    /// ([`quarantine_corrupt_captures`]). The caller runs the check again later.
    pub held: Vec<CorruptCapture>,
    /// Rows Tetrad could not check; the caller runs the check again later.
    pub unchecked: Vec<String>,
}

/// `0x702f70`: every Downloads capture (`0x6a1bb8`), then every user capture (`0x833e90`,
/// `type = 2`), in rowid order. `check(rawdata, metadata)` judges a row as stored (NULL reads
/// as empty). Each row that fails leaves its store (`0x6a48fc`, `0x6a1600`) for
/// [`tetrad_storage::stock::QUARANTINE_TABLE`] in one transaction (O: stock deletes it).
/// O: a read-only user row is not checked (stock checks it; `zenui.db` holds none).
///
/// O (review r23 K5, r25 Q7): stock corruption is a cut write, so it hits one capture or a
/// few. When every row judged fails (also a single one), or more than one fails and they are
/// more than a quarter of the rows judged, a Tetrad reader gap or a wrong cipher is more likely,
/// so no row moves: they all go to [`CorruptionCheck::held`].
pub fn quarantine_corrupt_captures(
    db: &mut StockDb,
    mut check: impl FnMut(&[u8], &[u8]) -> CaptureCheck,
) -> Result<CorruptionCheck> {
    let mut result = CorruptionCheck::default();
    let mut sound = 0;
    {
        let view = db.view();
        let mut rows = Vec::new();
        for id in view.download_ids(DownloadKind::Capture)? {
            if let Some(row) = view.download(DownloadKind::Capture, &id)? {
                rows.push((CaptureStore::Downloads, id, row.raw_data, row.metadata));
            }
        }
        for id in view.product_ids(ProductType::Capture)? {
            if let Some(row) = view.product(&id)?.filter(|row| !row.read_only) {
                rows.push((CaptureStore::User, id, row.raw_data, row.metadata));
            }
        }
        for (store, id, raw, metadata) in rows {
            let raw = raw.unwrap_or_default();
            let metadata = metadata.unwrap_or_default();
            let (name, reason) = match check(&raw, &metadata) {
                CaptureCheck::Sound => {
                    sound += 1;
                    continue;
                }
                CaptureCheck::Unchecked(reason) => {
                    result.unchecked.push(format!("{id}: {reason}"));
                    continue;
                }
                CaptureCheck::BadMetadata(reason) => (id.clone(), reason),
                CaptureCheck::BadRawdata { name, reason } => {
                    (if name.is_empty() { id.clone() } else { name }, reason)
                }
            };
            result.removed.push(CorruptCapture {
                store,
                id,
                name,
                reason,
            });
        }
    }
    let failed = result.removed.len();
    if failed > 0 && (sound == 0 || (failed > 1 && 4 * failed > sound + failed)) {
        result.held = std::mem::take(&mut result.removed);
    }
    if !result.removed.is_empty() {
        db.transaction(|write| {
            result.removed.iter().try_for_each(|row| {
                write.quarantine_capture(row.store, &row.id, &row.name, &row.reason)
            })
        })?;
    }
    Ok(result)
}

/// Puts a capture of [`tetrad_storage::stock::QUARANTINE_TABLE`] back into its store (O, review
/// r23 K5; `StockWrite::restore_quarantined_capture`), for a later tool or Cortex Control
/// entry. A user capture needs room under [`MEDIA_LIBRARY_CAP`].
pub fn restore_quarantined_capture(db: &mut StockDb, store: CaptureStore, id: &str) -> Result<()> {
    Ok(db.transaction(|write| {
        if store == CaptureStore::User
            && write.product_ids(ProductType::Capture)?.len() >= MEDIA_LIBRARY_CAP
        {
            return Err(tetrad_storage::Error::Limit);
        }
        write.restore_quarantined_capture(store, id)
    })?)
}
