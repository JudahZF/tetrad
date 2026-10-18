//! The stored DOWNLOADS rows of the Directory views ([`super::super::Device::downloads_rows`]
//! without the queue rows), read on a worker thread with its own read-only `zenui.db`
//! connection, at boot and after each change of the Downloads tables (A3).
//!
//! The QC boot pass took 427 ms in `downloads_rows NeuralCapture` (r53 probe): the listing
//! reads `metadata`, which lies behind each row's 10 KB `rawdata` overflow pages, so SQLite
//! walks them for all 389 captures. The capture browser list (`ui/captures.rs`) reads the same
//! way. Until a read lands the views keep the rows they have (none at boot). I: stock fills its
//! Downloads models when their views are built; when is not traced.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use tetrad_media::factory::DeviceCipher;
use tetrad_services::cloud::services::ProductKind;
use tetrad_storage::stock::{DbLimits, DownloadKind, StockDb, StockView};

use super::DownloadsRow;

/// The stored rows of the two Downloads tables.
#[derive(Debug, Default)]
pub(in crate::device) struct StoredRows {
    presets: Vec<DownloadsRow>,
    captures: Vec<DownloadsRow>,
}

impl StoredRows {
    pub fn of(&self, kind: ProductKind) -> &[DownloadsRow] {
        match kind {
            ProductKind::Preset => &self.presets,
            ProductKind::NeuralCapture => &self.captures,
            ProductKind::ImpulseResponse => &[],
        }
    }
}

/// Where the next read comes from.
pub(in crate::device) enum Source<'a> {
    /// No open `zenui.db` (no library, or a restore holds it): no stored rows.
    None,
    /// The database file: a worker opens it read-only.
    File(PathBuf),
    /// A database without a file (in memory): read on this thread.
    Open(&'a StockDb),
}

#[derive(Debug, Default)]
pub(in crate::device) struct StoredDownloads {
    rows: Arc<StoredRows>,
    /// The Downloads change count ([`super::Services::downloads_changes`]) `rows` are of.
    read: Option<u64>,
    /// The read in flight and the change count it started at.
    job: Option<(u64, mpsc::Receiver<StoredRows>)>,
}

impl StoredDownloads {
    pub fn rows(&self) -> Arc<StoredRows> {
        Arc::clone(&self.rows)
    }

    /// Takes a finished read, then starts one when the rows are older than `changes` and
    /// none runs. Returns whether the rows changed (the views rebuild).
    pub fn poll(
        &mut self,
        changes: u64,
        source: Source<'_>,
        cipher: Option<&DeviceCipher>,
    ) -> bool {
        let mut landed = false;
        if let Some((at, job)) = &self.job {
            let read = match job.try_recv() {
                Ok(rows) => rows,
                Err(mpsc::TryRecvError::Empty) => return false,
                // A read that stopped lists nothing until the next change.
                Err(mpsc::TryRecvError::Disconnected) => StoredRows::default(),
            };
            (self.rows, self.read, self.job) = (Arc::new(read), Some(*at), None);
            landed = true;
        }
        if self.read == Some(changes) {
            return landed;
        }
        let rows = match source {
            Source::None => StoredRows::default(),
            Source::Open(db) => read(&db.view(), cipher),
            Source::File(path) => {
                let (send, receive) = mpsc::channel();
                let (file, worker_cipher) = (path.clone(), cipher.cloned());
                let spawned = std::thread::Builder::new()
                    .name("downloads-rows".into())
                    .spawn(move || _ = send.send(read_file(&file, worker_cipher.as_ref())));
                match spawned {
                    Ok(_) => {
                        self.job = Some((changes, receive));
                        return landed;
                    }
                    // No thread: read here.
                    Err(_) => read_file(&path, cipher),
                }
            }
        };
        (self.rows, self.read) = (Arc::new(rows), Some(changes));
        true
    }
}

fn read_file(path: &Path, cipher: Option<&DeviceCipher>) -> StoredRows {
    StockDb::open(path, DbLimits::default(), true)
        .map(|db| read(&db.view(), cipher))
        .unwrap_or_default()
}

fn read(view: &StockView<'_>, cipher: Option<&DeviceCipher>) -> StoredRows {
    StoredRows {
        presets: stored_rows(view, DownloadKind::Preset, cipher),
        captures: stored_rows(view, DownloadKind::Capture, cipher),
    }
}

/// The rows of one Downloads table in table order (`0x7ea828`: `SELECT id, metadata, hash
/// FROM downloads_product_nc`; presets `0x6a25b0`), with name, author and version from their
/// metadata (a capture's in the device `cipher`, as `0x6f6484` reads it). O: a row whose
/// metadata does not read is listed by its id; a table that does not read lists nothing.
pub(in crate::device) fn stored_rows(
    view: &StockView<'_>,
    table: DownloadKind,
    cipher: Option<&DeviceCipher>,
) -> Vec<DownloadsRow> {
    /// The metadata keys a row shows; serde skips the others unbuilt.
    #[derive(serde::Deserialize)]
    struct Shown {
        name: Option<serde_json::Value>,
        author: Option<serde_json::Value>,
        version: Option<serde_json::Value>,
    }
    let cipher = cipher.filter(|_| table == DownloadKind::Capture);
    let listing = view.download_listing(table).unwrap_or_default();
    (listing.into_iter())
        .map(|stored| {
            let mut metadata = stored.metadata.unwrap_or_default();
            if let Some(cipher) = cipher {
                cipher.apply(&mut metadata);
            }
            let shown = serde_json::from_slice::<Shown>(&metadata).ok();
            let text = |value: fn(&Shown) -> &Option<serde_json::Value>| {
                (shown.as_ref())
                    .and_then(|shown| value(shown).as_ref()?.as_str())
                    .map(str::to_owned)
            };
            DownloadsRow {
                name: text(|shown| &shown.name).unwrap_or_else(|| stored.id.clone()),
                author: text(|shown| &shown.author).unwrap_or_default(),
                version: text(|shown| &shown.version).unwrap_or_default(),
                id: stored.id,
                hash: stored.hash,
                stored: true,
            }
        })
        .collect()
}
