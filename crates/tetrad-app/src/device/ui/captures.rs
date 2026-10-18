//! The Add device capture browser list ([`tetrad_ui::CaptureLibrary`]): what the stock browser
//! (`zenUI::VirtualDevicesCaptures`, ctor `0x841f60`) reads through the capture library.
//!
//! - Captures Library (`0x765520`, library `+0x18`): the `factory.db` captures with their
//!   folder tree ("Factory Captures V1" / "V2", device type folders, device folders; built at
//!   first boot by `0x6e86fc`), then the `zenui.db` user captures and their folders ("My
//!   Captures"). On the B7 unit: 2062 factory + 42 user = 2104 (shot sn-4).
//! - Downloads (`0x7657f8`, `VDCDownloadsNCModel`): `downloads_product_nc` rows; the block
//!   key is the row's hash (`0x214b84`).
//!
//! Metadata reads with the device cipher (factory captures too, as `MediaLibrary` loads them);
//! a row whose metadata does not read is left out. The list is read on a worker thread with
//! its own read-only connections, from boot on and again after each media change, so the
//! browser opens on a ready list. On the QC the first read took about 10 s on the UI thread
//! (B7 re-check): the metadata column follows the rawdata blob, so SQLite walks every overflow
//! page of the 32 MB `factory.db`. The `factory.db` part is read once (it is read-only). I: the
//! stock list model is built when the device list is built, not per open.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use tetrad_library::sidecar::Sidecar;
use tetrad_library::stock::media_query::CaptureFacts;
use tetrad_media::factory::DeviceCipher;
use tetrad_storage::stock::{DbLimits, DownloadKind, ProductType, StockDb, StockView};
use tetrad_ui::{
    CaptureDevice, CaptureLibrary, CaptureOrigin, LibraryCapture, LibraryFolder, MediaEntry,
};

#[derive(Debug, Default)]
pub(in crate::device) struct Captures {
    factory: Option<Arc<CaptureLibrary>>,
    library: Arc<CaptureLibrary>,
    /// The media list the last read started for: a new list (a media change) reads again.
    read_for: Option<Arc<Vec<MediaEntry>>>,
    job: Option<mpsc::Receiver<Read>>,
}

/// The databases a read opens.
#[derive(Debug, Clone, Default)]
pub(in crate::device) struct Sources {
    pub user: Option<PathBuf>,
    pub factory: Option<PathBuf>,
}

/// What a worker read gives back.
struct Read {
    factory: Option<Arc<CaptureLibrary>>,
    library: CaptureLibrary,
    failures: Vec<String>,
}

impl Captures {
    /// The list the snapshot shares: the same `Arc` until the next read lands.
    pub fn library(&self) -> Arc<CaptureLibrary> {
        Arc::clone(&self.library)
    }

    /// REFRESH: the next [`Self::refresh`] reads again (stock `0x43432c` resets the model and
    /// fetches it again).
    pub fn reread(&mut self) {
        self.read_for = None;
    }

    /// Takes a finished read, then starts one when `media` is a new list (boot, a media
    /// change) and none runs. Returns a line per store that could not be read; that part is
    /// left out.
    pub fn refresh(
        &mut self,
        media: &Arc<Vec<MediaEntry>>,
        sources: Sources,
        cipher: Option<&DeviceCipher>,
    ) -> Vec<String> {
        let mut failures = Vec::new();
        if let Some(job) = &self.job {
            match job.try_recv() {
                Ok(read) => {
                    self.factory = read.factory.or(self.factory.take());
                    self.library = Arc::new(read.library);
                    failures = read.failures;
                    self.job = None;
                }
                Err(mpsc::TryRecvError::Empty) => return failures,
                Err(mpsc::TryRecvError::Disconnected) => {
                    failures.push("capture list: the read stopped".into());
                    self.job = None;
                }
            }
        }
        let new = (self.read_for.as_ref()).is_none_or(|seen| !Arc::ptr_eq(seen, media));
        let Some(cipher) = cipher.filter(|_| new) else {
            return failures;
        };
        self.read_for = Some(Arc::clone(media));
        let (send, receive) = mpsc::channel();
        let (known, cipher) = (self.factory.clone(), cipher.clone());
        let spawned = std::thread::Builder::new()
            .name("capture-list".into())
            .spawn(move || _ = send.send(read(sources, known, &cipher)));
        match spawned {
            Ok(_) => self.job = Some(receive),
            Err(error) => failures.push(format!("capture list: {error}")),
        }
        failures
    }
}

/// The whole list: the factory part (`known`, else read now), then the user captures and
/// Downloads.
fn read(sources: Sources, known: Option<Arc<CaptureLibrary>>, cipher: &DeviceCipher) -> Read {
    let mut failures = Vec::new();
    let factory = match (known, sources.factory) {
        (Some(known), _) => Some(known),
        (None, Some(path)) => match StockDb::open_factory(&path, DbLimits::default())
            .and_then(|db| library_part(&db.view(), CaptureOrigin::Factory, cipher))
        {
            Ok(part) => Some(Arc::new(part)),
            Err(error) => {
                failures.push(format!("factory captures: {error}"));
                None
            }
        },
        (None, None) => None,
    };
    let mut library = factory.as_deref().cloned().unwrap_or_default();
    if let Some(path) = sources.user {
        match StockDb::open(&path, DbLimits::default(), true) {
            Ok(db) => {
                let view = db.view();
                match library_part(&view, CaptureOrigin::User, cipher) {
                    Ok(part) => append(&mut library, part),
                    Err(error) => failures.push(format!("user captures: {error}")),
                }
                match downloads(&view, cipher) {
                    Ok(rows) => library.captures.extend(rows),
                    Err(error) => failures.push(format!("downloaded captures: {error}")),
                }
            }
            Err(error) => failures.push(format!("zenui.db: {error}")),
        }
    }
    Read {
        factory,
        library,
        failures,
    }
}

/// The capture folders and capture products of one database; folder indices are local.
fn library_part(
    view: &StockView<'_>,
    origin: CaptureOrigin,
    cipher: &DeviceCipher,
) -> tetrad_storage::Result<CaptureLibrary> {
    let stored: Vec<_> = (view.folders()?.into_iter())
        .filter(|folder| folder.product_type == ProductType::Capture)
        .collect();
    let index: BTreeMap<i64, u16> = (stored.iter().map(|folder| folder.id))
        .zip(0u16..)
        .collect();
    let mut members = BTreeMap::<String, Vec<u16>>::new();
    for (folder, at) in stored.iter().zip(0u16..) {
        for id in view.folder_products(folder.id)? {
            members.entry(id).or_default().push(at);
        }
    }
    let folders = (stored.into_iter())
        .map(|folder| LibraryFolder {
            parent: index.get(&folder.parent).copied(),
            name: folder.name,
        })
        .collect();
    let captures = (view.product_listing(ProductType::Capture)?.into_iter())
        .filter_map(|product| {
            let folders = members.remove(&product.id).unwrap_or_default();
            capture(product.id, product.metadata, origin, folders, cipher)
        })
        .collect();
    Ok(CaptureLibrary { folders, captures })
}

/// The Downloads rows that have a hash (the block key; `capture_record` refuses the others).
fn downloads(
    view: &StockView<'_>,
    cipher: &DeviceCipher,
) -> tetrad_storage::Result<Vec<LibraryCapture>> {
    Ok((view.download_listing(DownloadKind::Capture)?.into_iter())
        .filter_map(|row| {
            let hash = row.hash.filter(|hash| !hash.is_empty())?;
            capture(
                hash,
                row.metadata,
                CaptureOrigin::Download,
                Vec::new(),
                cipher,
            )
        })
        .collect())
}

fn capture(
    value: String,
    metadata: Option<Vec<u8>>,
    origin: CaptureOrigin,
    folders: Vec<u16>,
    cipher: &DeviceCipher,
) -> Option<LibraryCapture> {
    let mut plain = metadata?;
    cipher.apply(&mut plain);
    let facts = CaptureFacts::from_metadata(&value, &plain).ok()?;
    let date = Sidecar::parse(&plain)
        .ok()?
        .text("date")
        .unwrap_or_default()
        .to_owned();
    Some(LibraryCapture {
        device: (facts.device).and_then(|code| CaptureDevice::from_code(u8::try_from(code).ok()?)),
        nc2: facts.version == "2",
        name: facts.name,
        date,
        origin,
        folders,
        value,
    })
}

/// `part` after `library`: its folder indices move past the folders already listed.
fn append(library: &mut CaptureLibrary, part: CaptureLibrary) {
    let Ok(base) = u16::try_from(library.folders.len()) else {
        return;
    };
    let shift = |index: u16| index.saturating_add(base);
    library
        .folders
        .extend(part.folders.into_iter().map(|folder| LibraryFolder {
            parent: folder.parent.map(shift),
            ..folder
        }));
    library
        .captures
        .extend(part.captures.into_iter().map(|capture| LibraryCapture {
            folders: capture.folders.into_iter().map(shift).collect(),
            ..capture
        }));
}
