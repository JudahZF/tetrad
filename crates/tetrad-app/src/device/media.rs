//! The app IR library for the cab and IR Loader drivers (w2b-dsp merge request M4).
//!
//! - Factory IRs: the entity catalog `/opt/neuraldsp/factory_ir_entities.pb`; a cab
//!   `ir selector` (`<tag>_<cab>_<mic>`) names an entity whose files (4,096-byte NDSP images
//!   under `impulse_responses`) are decrypted with the device cipher, in catalog order
//!   (`0x3289c4`, `0x3284fc`).
//! - User IRs: `zenui.db` type-1 products named by their `CIR_…` id (`0x327688`, `0x5599a0`);
//!   rawdata are the samples, metadata is device-encrypted.
//!
//! - Captures: a capture block `file_name` cut to 64 UTF-16 units names a user product (type
//!   2) or a cloud download (`0x6f6484`: user, then downloads). Metadata and V1 rawdata use
//!   the device cipher (ZC mode 1, `0x7efc38`, `0x7ed2c8`); NC2 rawdata (`version` "2") uses
//!   the DSP boot vector `0x27` (ZC mode 2, controller `+0xf04c`), set by
//!   [`MediaLibrary::set_nc2_key`]. Both keys are run-time inputs and stay in memory.
//!
//! Values are resolved before a push and kept for the process in one [`ResolvedMedia`] that
//! the device borrows ([`MediaLibrary::resolve`]); nothing is copied per push. A user IR id
//! or capture key that `zenui.db` does not hold is looked up again at the next push, so a
//! later import or save is seen. The device reads the files through its platform.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::Arc;

use tetrad_capture::stored::{CaptureMetadata, RecordVersion, v1_runtime_data};
use tetrad_capture::v2::Capture2File;
use tetrad_dsp_host::setup::drivers::media::{CabIrs, CaptureData};
use tetrad_library::stock::media_query::CaptureFacts;
use tetrad_library::stock::{CaptureCheck, CaptureSource, find_capture};
use tetrad_media::cab::cab_ir;
use tetrad_media::catalog::{CatalogError, CatalogLimits, FactoryIrCatalog};
use tetrad_media::factory::DeviceCipher;
use tetrad_media::user::{IrReference, UserIrMetadata, UserIrProduct};
use tetrad_storage::stock::{
    CaptureEncryption, CaptureStore, DownloadKind, ProductType, StockDb, StockView,
};
use tetrad_ui::{MAX_MEDIA_ITEMS, MediaEntry, MediaFolder};

use super::dsp::ResolvedMedia;
use super::platform::{DspFile, ServiceBackends as _};
use tetrad_services::exec::Files as _;

/// Capture manager markers (`0x704198`; paths from the `_INIT` at `0xafd8c`). A 4.1.0 unit
/// has both "done" markers, so boot runs neither step; the restore reload (`0x3fcf64`) touches
/// the translation flag and removes both "done" markers (`0x7038a8`, `0x6f6218`, `0x6f5fa0`).
pub const CAPTURE_TRANSLATION_FLAG: &str = "/etc/userCaptureTranslationFlag";
pub const CAPTURE_CORRUPTION_CHECKED: &str = "/etc/userCaptureCorruptionCheckFlag";
pub const CAPTURE_CONVERSION_FINISHED: &str = "/opt/neuraldsp/captureConversionFinished";
pub const CONVERSION_DELETED_NAMES: &str = "/opt/neuraldsp/conversionDeletedNameCache";
/// Owned journal of the legacy format conversion (O, no stock file): a JSON object, old file
/// name → hash, for the capture whose converted pair is staged and not yet committed.
pub const CAPTURE_CONVERSION_JOURNAL: &str = "/opt/neuraldsp/zenCaptureConversionJournal";

/// Owned bounds above the pinned catalog (690 entities, 11,515 files).
const CATALOG_LIMITS: CatalogLimits = CatalogLimits {
    max_bytes: 4 << 20,
    max_entities: 4096,
    max_files_per_entity: 64,
};

/// `QString::left(64)` of a capture `file_name` (VA `0x517cac`).
const CAPTURE_KEY_UNITS: usize = 64;

pub struct MediaLibrary {
    catalog: FactoryIrCatalog,
    /// ZC mode 1 (seed ‖ serial): factory IRs, user IR metadata, capture metadata, V1 rawdata.
    cipher: DeviceCipher,
    /// ZC mode 2 over the DSP boot vector `0x27`: NC2 capture rawdata.
    nc2: Option<DeviceCipher>,
    /// Every value loaded so far, for any plan.
    resolved: ResolvedMedia,
    /// Values that are loaded, absent from the factory catalog or failed (failures are
    /// reported once).
    settled: HashSet<String>,
    /// `factory.db` (read-only): the factory captures a preset can name.
    factory: Option<StockDb>,
    /// The `deviceType` of each loaded capture, under the same keys as the resolved
    /// captures. Stock keeps it in model word `[0x38]` for the Grid glyph, colour and badge.
    devices: BTreeMap<String, tetrad_ui::CaptureDevice>,
}

impl MediaLibrary {
    /// The entity catalog bytes and the device cipher (ZenUI seed + serial).
    pub fn new(catalog: &[u8], cipher: DeviceCipher) -> Result<Self, CatalogError> {
        Ok(Self {
            catalog: FactoryIrCatalog::decode(catalog, CATALOG_LIMITS)?,
            cipher,
            nc2: None,
            resolved: ResolvedMedia::default(),
            settled: HashSet::new(),
            factory: None,
            devices: BTreeMap::new(),
        })
    }

    /// The factory capture store (`factory.db`, read-only).
    pub fn set_factory(&mut self, factory: StockDb) {
        self.factory = Some(factory);
    }

    /// Resolves block string values: factory selectors to their entity files, user IR ids
    /// to their product, other strings to a capture record when the library holds one (under
    /// the value and its 64-unit key). Unknown IR values resolve to nothing (stock: "Cannot
    /// open IR", the default IR). Returns every value loaded so far (a superset of `values`)
    /// and one line per value that failed to load, once per process.
    pub fn resolve<'v>(
        &mut self,
        values: impl IntoIterator<Item = &'v str>,
        mut read: impl FnMut(DspFile<'_>) -> Result<Vec<u8>, String>,
        database: Option<&StockDb>,
    ) -> (&ResolvedMedia, Vec<String>) {
        let mut failures = Vec::new();
        for value in values {
            if self.settled.contains(value) {
                continue;
            }
            let settled = match IrReference::classify(value) {
                IrReference::Unexpected(_) => match self.load_capture(value, database) {
                    Ok(Some((capture, device))) => {
                        let key = capture_key(value);
                        if key != value {
                            self.resolved
                                .captures
                                .insert(value.to_owned(), capture.clone());
                        }
                        self.resolved.captures.insert(key.to_owned(), capture);
                        if let Some(device) = device {
                            self.devices.insert(value.to_owned(), device);
                            self.devices.insert(key.to_owned(), device);
                        }
                        true
                    }
                    Ok(None) => false,
                    Err(error) => {
                        failures.push(format!("capture {value:?}: {error}"));
                        true
                    }
                },
                reference => match self.load(value, &mut read, database) {
                    Ok(Some(irs)) => {
                        self.resolved.cabs.insert(value.to_owned(), irs);
                        true
                    }
                    Ok(None) => !matches!(reference, IrReference::Custom(_)),
                    Err(error) => {
                        failures.push(format!("IR {value:?}: {error}"));
                        true
                    }
                },
            };
            if settled {
                self.settled.insert(value.to_owned());
            }
        }
        (&self.resolved, failures)
    }

    /// The DSP boot vector `0x27` (stock controller `+0xf04c`, read by `0x515a48` for every
    /// NC2 record load). Without it NC2 records fail to load; V1 records do not need it.
    pub fn set_nc2_key(&mut self, key: &[u8]) {
        self.nc2 = Some(DeviceCipher::caller_secret(key));
    }

    /// The NC2 key from DSP boot vector `0x27`, once received.
    pub fn nc2_cipher(&self) -> Option<&DeviceCipher> {
        self.nc2.as_ref()
    }

    /// The device cipher (ZC mode 1): Cortex Control reads capture metadata with it.
    pub fn cipher(&self) -> &DeviceCipher {
        &self.cipher
    }

    /// One stored capture as the corruption check (`0x702f70`) reads it: the metadata
    /// through `0x7efc38`, then the rawdata through `0x7ed2c8`, as `Self::load_capture`
    /// does. An NC2 record before the DSP boot vector `0x27` is not checked (O).
    pub fn check_capture(&self, raw: &[u8], metadata: &[u8]) -> CaptureCheck {
        let metadata = match self.capture_metadata(metadata.to_vec()) {
            Ok(metadata) => metadata,
            Err(error) => return CaptureCheck::BadMetadata(error),
        };
        let mut raw = raw.to_vec();
        let loaded = match metadata.version {
            RecordVersion::V1 => {
                self.cipher.apply(&mut raw);
                v1_runtime_data(&raw)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
            RecordVersion::Nc2 => {
                let Some(nc2) = &self.nc2 else {
                    return CaptureCheck::Unchecked("an NC2 record before DSP vector 0x27".into());
                };
                nc2.apply(&mut raw);
                Capture2File::parse(&raw)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
        };
        match loaded {
            Ok(()) => CaptureCheck::Sound,
            Err(reason) => CaptureCheck::BadRawdata {
                name: metadata.name,
                reason,
            },
        }
    }

    /// Names for the UI media picker, never IR data: factory entities in catalog order
    /// (name = readable name, value = `ir selector`), then `zenui.db` user IRs and user
    /// captures ("My Captures", product type 2), each named from its decrypted metadata with
    /// the product id as value. Returns one line per product whose name cannot be read; that
    /// product is left out.
    pub fn entries(&self, database: Option<&StockDb>) -> (Vec<MediaEntry>, Vec<String>) {
        let mut entries: Vec<MediaEntry> = self
            .catalog
            .entities()
            .iter()
            .map(|entity| MediaEntry {
                folder: MediaFolder::FactoryIrs,
                name: entity.readable_name.clone(),
                value: entity.selector(),
            })
            .collect();
        let mut failures = Vec::new();
        if let Some(database) = database {
            let view = database.view();
            match view.product_rows(ProductType::Ir) {
                Ok(rows) => {
                    for (id, row) in rows {
                        let name = row.map_err(|error| error.to_string()).and_then(|row| {
                            UserIrMetadata::from_blob(
                                &row.metadata.unwrap_or_default(),
                                &self.cipher,
                            )
                            .map_err(|error| error.to_string())
                        });
                        match name {
                            Ok(metadata) => entries.push(MediaEntry {
                                folder: MediaFolder::MyIrs,
                                name: metadata.name,
                                value: id,
                            }),
                            Err(error) => failures.push(format!("IR {id:?}: {error}")),
                        }
                    }
                }
                Err(error) => failures.push(format!("user IRs: {error}")),
            }
            match self.capture_entries(&view) {
                Ok((captures, unreadable)) => {
                    entries.extend(captures);
                    failures.extend(unreadable);
                }
                Err(error) => failures.push(format!("user captures: {error}")),
            }
        }
        entries.truncate(MAX_MEDIA_ITEMS);
        (entries, failures)
    }

    /// "My Captures": user capture products in `zenui.db` order.
    fn capture_entries(
        &self,
        view: &StockView<'_>,
    ) -> Result<(Vec<MediaEntry>, Vec<String>), String> {
        local_store(view, CaptureStore::User)?;
        let rows = view
            .product_rows(ProductType::Capture)
            .map_err(|error| error.to_string())?;
        let mut entries = Vec::new();
        let mut failures = Vec::new();
        for (id, row) in rows {
            let name = row
                .map_err(|error| error.to_string())
                .and_then(|row| self.capture_metadata(row.metadata.unwrap_or_default()));
            match name {
                Ok(metadata) => entries.push(MediaEntry {
                    folder: MediaFolder::MyCaptures,
                    name: metadata.name,
                    value: id,
                }),
                Err(error) => failures.push(format!("capture {id:?}: {error}")),
            }
        }
        Ok((entries, failures))
    }

    /// The `deviceType` of the loaded captures by block value ([`Self::resolve`]).
    pub fn capture_devices(&self) -> &BTreeMap<String, tetrad_ui::CaptureDevice> {
        &self.devices
    }

    /// A capture record by block value (`0x6f6484` → `0x7ed2c8`) and its metadata
    /// `deviceType`. `None`: no record holds the key (stock sends nothing for the block).
    fn load_capture(
        &self,
        value: &str,
        database: Option<&StockDb>,
    ) -> Result<Option<(CaptureData, Option<tetrad_ui::CaptureDevice>)>, String> {
        let Some(database) = database else {
            return Ok(None);
        };
        let view = database.view();
        let factory = self.factory.as_ref().map(StockDb::view);
        let (raw, metadata) = match find_capture(&view, factory.as_ref(), capture_key(value))
            .map_err(|error| error.to_string())?
        {
            Some(CaptureSource::User(product)) => {
                local_store(&view, CaptureStore::User)?;
                (product.raw_data, product.metadata)
            }
            Some(CaptureSource::Download(download)) => {
                local_store(&view, CaptureStore::Downloads)?;
                (download.raw_data, download.metadata)
            }
            // Factory captures read with the device cipher as user captures do (C, S3 on the
            // QC 2026-10-15: Royal Blood's two factory captures loaded). `factory.db` has no
            // capture store flag.
            Some(CaptureSource::Factory(product)) => (product.raw_data, product.metadata),
            None => return Ok(None),
        };
        let mut plain = metadata.unwrap_or_default();
        self.cipher.apply(&mut plain);
        let metadata = CaptureMetadata::from_json(&plain).map_err(|error| error.to_string())?;
        // Factory captures carry it too (`factory.db` 4.1.0: all 2062 records).
        let device = (CaptureFacts::from_metadata(value, &plain).ok())
            .and_then(|facts| facts.device)
            .and_then(|code| tetrad_ui::CaptureDevice::from_code(u8::try_from(code).ok()?));
        let mut raw = raw.ok_or("the record has no rawdata")?;
        let data = match metadata.version {
            RecordVersion::V1 => {
                self.cipher.apply(&mut raw);
                CaptureData::V1(Arc::from(
                    v1_runtime_data(&raw).map_err(|error| error.to_string())?,
                ))
            }
            RecordVersion::Nc2 => {
                self.nc2
                    .as_ref()
                    .ok_or("NC2 record: the DSP boot vector 0x27 was not received")?
                    .apply(&mut raw);
                let file = Capture2File::parse(&raw).map_err(|error| error.to_string())?;
                CaptureData::V2(Arc::from(file.sharc_blob()))
            }
        };
        Ok(Some((data, device)))
    }

    pub(in crate::device) fn capture_metadata(
        &self,
        mut metadata: Vec<u8>,
    ) -> Result<CaptureMetadata, String> {
        self.cipher.apply(&mut metadata);
        CaptureMetadata::from_json(&metadata).map_err(|error| error.to_string())
    }

    /// `None`: the value names nothing loadable.
    fn load(
        &mut self,
        value: &str,
        read: &mut impl FnMut(DspFile<'_>) -> Result<Vec<u8>, String>,
        database: Option<&StockDb>,
    ) -> Result<Option<CabIrs>, String> {
        match IrReference::classify(value) {
            IrReference::Factory { tag, .. } => {
                let Some(entity) = self.catalog.by_selector(value) else {
                    return Ok(None);
                };
                let irs = entity
                    .files
                    .iter()
                    .map(|file| {
                        let bytes = read(DspFile::FactoryIr(&file.path))?;
                        let samples = self
                            .cipher
                            .decode_factory_ir(&bytes)
                            .map_err(|error| format!("{}: {error}", file.path))?;
                        Ok(cab_ir(&samples))
                    })
                    .collect::<Result<_, String>>()?;
                Ok(Some(CabIrs::Factory {
                    tag: tag.to_owned(),
                    irs,
                }))
            }
            IrReference::Custom(id) => {
                let database = database.ok_or("zenui.db is not open")?;
                let Some(product) = database
                    .view()
                    .product(id)
                    .map_err(|error| error.to_string())?
                else {
                    return Ok(None);
                };
                let product = UserIrProduct::from_row(
                    id,
                    product.raw_data.as_deref().unwrap_or_default(),
                    product.metadata.as_deref().unwrap_or_default(),
                    &self.cipher,
                )
                .map_err(|error| error.to_string())?;
                let ir = cab_ir(&product.samples);
                self.resolved.irs.insert(value.to_owned(), product.samples);
                Ok(Some(CabIrs::Custom(ir)))
            }
            IrReference::Empty | IrReference::Unexpected(_) => Ok(None),
        }
    }
}

/// Boot re-encrypts a capture store flagged global for this device and flags it local
/// (`Device::migrate_boot_media`, `0x6a6f78`). A store still flagged global was not translated
/// (no `--persist`, or a failure kept it), so it is refused. An unflagged store is
/// device-keyed ([`CaptureEncryption::is_device_keyed`]).
fn local_store(view: &StockView<'_>, store: CaptureStore) -> Result<(), String> {
    match view
        .capture_encryption(store)
        .map_err(|error| error.to_string())?
    {
        flag if flag.is_device_keyed() => Ok(()),
        other => Err(format!(
            "{store:?} capture store is {other:?}: its device translation is not implemented"
        )),
    }
}

/// The capture stores that hold rows and are flagged global: the boot translation
/// (`0x6a6f78`) has not re-encrypted them for this device, so their rows do not read with the
/// device cipher. Stock translates only a global store at boot; an unset store is
/// device-keyed already (C, B13). An empty store needs no translation, whatever its flag.
fn untranslated_stores(
    view: &StockView<'_>,
) -> tetrad_storage::Result<Vec<(CaptureStore, CaptureEncryption)>> {
    use tetrad_storage::stock::DownloadKind;
    let mut stores = Vec::new();
    for store in [CaptureStore::Downloads, CaptureStore::User] {
        let flag = view.capture_encryption(store)?;
        let rows = match store {
            CaptureStore::Downloads => view.download_ids(DownloadKind::Capture)?,
            CaptureStore::User => view.product_ids(ProductType::Capture)?,
        };
        if flag == CaptureEncryption::Global && !rows.is_empty() {
            stores.push((store, flag));
        }
    }
    Ok(stores)
}

/// The first 64 UTF-16 units of a capture `file_name` (`QString::left(64)`, VA `0x517cac`).
/// Owned: a surrogate pair is never split.
fn capture_key(value: &str) -> &str {
    let mut units = 0;
    let end = value
        .char_indices()
        .find(|(_, c)| {
            units += c.len_utf16();
            units > CAPTURE_KEY_UNITS
        })
        .map_or(value.len(), |(at, _)| at);
    &value[..end]
}

/// Metadata for Directory sort roles and SQLite folder membership. No payload and no cipher
/// is exposed to tetrad-ui; metadata remains device-encrypted on disk. Captures also carry the
/// plain `deviceType`, `gainType` and `version` for the capture filter and the Device Type
/// and Gain sorts (`0x7ee62c` case 2).
impl MediaLibrary {
    pub fn directory_media(
        &self,
        database: Option<&StockDb>,
    ) -> Result<Vec<tetrad_ui::DirectoryMedia>, super::DeviceError> {
        let Some(db) = database else {
            return Ok(Vec::new());
        };
        let view = db.view();
        let folders = view.folders()?;
        let mut memberships = std::collections::BTreeMap::<String, Vec<i64>>::new();
        for folder in &folders {
            for id in view.folder_products(folder.id)? {
                memberships.entry(id).or_default().push(folder.id);
            }
        }
        let mut entries = Vec::new();
        for kind in [ProductType::Capture, ProductType::Ir] {
            for (id, row) in view.product_rows(kind)? {
                let row = row?;
                // Health r34 #2: a row whose metadata does not read is left out, so the
                // others keep their details; `entries` names it in a notice.
                let Ok((metadata, plain)) =
                    self.directory_metadata(row.metadata.unwrap_or_default())
                else {
                    continue;
                };
                let capture = (kind == ProductType::Capture)
                    .then(|| CaptureFacts::from_metadata(&id, &plain).ok())
                    .flatten()
                    .map(|facts| tetrad_ui::CaptureMeta {
                        device: facts.device.and_then(|code| u8::try_from(code).ok()),
                        gain: facts.gain.and_then(|gain| u8::try_from(gain).ok()),
                        version: facts.version,
                        instrument: facts.instrument.and_then(|code| u8::try_from(code).ok()),
                    });
                entries.push(tetrad_ui::DirectoryMedia {
                    value: id.clone(),
                    author: metadata.text("author").unwrap_or_default().to_owned(),
                    date: metadata.text("date").unwrap_or_default().to_owned(),
                    read_only: row.read_only,
                    folders: memberships.remove(&id).unwrap_or_default(),
                    capture,
                });
            }
        }
        Ok(entries)
    }

    /// The parsed metadata and its plain bytes: as stored when it parses, else decrypted.
    fn directory_metadata(
        &self,
        mut bytes: Vec<u8>,
    ) -> tetrad_library::Result<(tetrad_library::sidecar::Sidecar, Vec<u8>)> {
        if let Ok(sidecar) = tetrad_library::sidecar::Sidecar::parse(&bytes) {
            return Ok((sidecar, bytes));
        }
        self.cipher.apply(&mut bytes);
        Ok((tetrad_library::sidecar::Sidecar::parse(&bytes)?, bytes))
    }

    fn forget(&mut self, id: &str) {
        self.settled.retain(|value| capture_key(value) != id);
        self.resolved
            .captures
            .retain(|value, _| capture_key(value) != id);
        self.resolved.irs.remove(id);
        self.resolved.cabs.remove(id);
    }

    /// Writes the fields of `edit` into the stored metadata of one user capture or IR; the
    /// id, date, rawdata and unknown JSON keys stay. One transaction writes the re-encrypted
    /// blob, after every check passed.
    ///
    /// Stock: the capture edit (`metadataSaveSlot` `0x6901c8` → `0x68f134`) puts the name,
    /// device type (`0x7e3664`) and instrument (`0x7e3694`) into the user capture record
    /// (`0x765520` `vt+0x40`), then saves it (`vt+0x50`); Cortex Control File UPDATE
    /// (`0x241be4`) writes the name and the has-bit `0x4000` instrument / `0x8000` device the
    /// same way. A name that another user capture holds is refused ("Name already exists",
    /// `0x6f6238` searches the user capture store only, C for the dialog; O for Control,
    /// whose `0x241be4` does not check). Device and instrument are capture fields only.
    pub fn update(
        &mut self,
        db: &mut StockDb,
        kind: tetrad_ui::MediaKind,
        id: &str,
        edit: &MediaEdit,
    ) -> Result<(), super::DeviceError> {
        use tetrad_capture::stored::{device_type_name, instrument_type_name};
        let unsupported = |what| super::DeviceError::from(tetrad_library::Error::Unsupported(what));
        let capture = kind == tetrad_ui::MediaKind::Capture;
        if !capture && (edit.device.is_some() || edit.instrument.is_some()) {
            return Err(unsupported("an IR has no device type or instrument"));
        }
        if edit.device.is_some_and(|code| !(0..=8).contains(&code))
            || edit.instrument.is_some_and(|code| !(0..=5).contains(&code))
        {
            return Err(unsupported(
                "a capture device or instrument code out of range",
            ));
        }
        let view = db.view();
        let product = view
            .product(id)?
            .ok_or_else(|| tetrad_storage::Error::ProductMissing(id.to_owned()))?;
        if product.product_type != product_type(kind) {
            return Err(unsupported("media product type"));
        }
        if let Some(name) = &edit.name {
            let units = name.encode_utf16().count();
            let max = if capture { 20 } else { 60 };
            if !(4..=max).contains(&units) || name.chars().any(char::is_control) {
                return Err(unsupported("media name length or characters"));
            }
            if capture && self.capture_named(&view, name, id)? {
                return Err(tetrad_library::Error::NameTaken(name.clone()).into());
            }
        }
        let (mut metadata, _) = self.directory_metadata(product.metadata.unwrap_or_default())?;
        if let Some(name) = &edit.name {
            metadata.set_text("name", name);
        }
        if let Some(device) = edit.device {
            metadata.set_text("deviceType", device_type_name(device));
        }
        if let Some(instrument) = edit.instrument {
            metadata.set_text("instrumentType", instrument_type_name(instrument));
        }
        let mut blob = metadata.encode();
        self.cipher.apply(&mut blob);
        db.transaction(|tx| tx.set_product_metadata(id, Some(&blob)))?;
        Ok(())
    }

    /// Whether a user capture other than `id` is named `name` (`0x6f6238`, I: exact text). A
    /// capture whose metadata does not read is skipped.
    pub(in crate::device) fn capture_named(
        &self,
        view: &StockView<'_>,
        name: &str,
        id: &str,
    ) -> Result<bool, super::DeviceError> {
        for other in view.product_ids(ProductType::Capture)? {
            if other == id {
                continue;
            }
            let metadata = view.product(&other)?.and_then(|row| row.metadata);
            let named = self
                .capture_metadata(metadata.unwrap_or_default())
                .is_ok_and(|metadata| metadata.name == name);
            if named {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// The metadata fields one edit writes ([`MediaLibrary::update`]); `None` keeps the stored
/// value. `device` and `instrument` are the stock codes of `deviceType` (0..=8, `0x7ecc18`)
/// and `instrumentType` (0..=5, `0x7ec4c4`); only a capture has them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaEdit {
    pub name: Option<String>,
    pub device: Option<i32>,
    pub instrument: Option<i32>,
}

fn product_type(kind: tetrad_ui::MediaKind) -> ProductType {
    match kind {
        tetrad_ui::MediaKind::Capture => ProductType::Capture,
        tetrad_ui::MediaKind::Ir => ProductType::Ir,
    }
}

fn collection(kind: tetrad_ui::MediaKind) -> tetrad_storage::media::Collection {
    match kind {
        tetrad_ui::MediaKind::Capture => tetrad_storage::media::Collection::Captures,
        tetrad_ui::MediaKind::Ir => tetrad_storage::media::Collection::Irs,
    }
}

/// Whether the folder key of a capture or IR favorite/recent names a folder that holds the
/// item, as `0x7d0224` checks it: `0x76bc68` finds the folder by key, then the folder's
/// `contains` (`0x83a268` for a database folder: its product set). Keys are the local root
/// (`local_nc_root` `0x8297e0`, `local_ir_root` `0x829440`) or, for a database folder,
/// `0x766a20` `"<id>_<c>"` with `c` the first character of the Qt connection name: `q` for
/// zenui.db (`qt_sql_default_connection`, `0x6a576c`), `f` for factory.db (`"factory"`,
/// `0x8328a4`). Tetrad lists no factory.db folders, so `_f` keys are skipped (I). The root holds
/// whatever the media list holds; the caller checks that.
fn media_folder_holds(
    db: Option<&StockDb>,
    folders: &[tetrad_ui::DirectoryFolder],
    kind: tetrad_ui::MediaKind,
    entry: &tetrad_library::stock::ItemRef,
) -> bool {
    let root = match kind {
        tetrad_ui::MediaKind::Capture => "local_nc_root",
        tetrad_ui::MediaKind::Ir => "local_ir_root",
    };
    if entry.folder == root {
        return true;
    }
    let Some(folder) = folders
        .iter()
        .find(|folder| folder.kind == kind && entry.folder == format!("{}_q", folder.id))
    else {
        return false;
    };
    db.is_some_and(|db| {
        db.view()
            .folder_products(folder.id)
            .is_ok_and(|products| products.contains(&entry.item))
    })
}

/// Stock plugin folder names (`/opt/neuraldsp/Plugins`, CorOS 4.1.0) and their ModelRepo
/// `plugin_id`. The ids match by the model names of each plugin (RE `plugin-tag-20261017`).
const PLUGIN_FOLDERS: [(&str, u32); 12] = [
    ("Fortin Nameless Suite X", 1),
    ("Archetype: Plini X", 3),
    ("Parallax X", 4),
    ("Archetype: Nolly X", 5),
    ("Archetype: Cory Wong X", 9),
    ("Archetype: Gojira X", 10),
    ("Soldano SLO-100 X", 11),
    ("Archetype: Tim Henson X", 12),
    ("Archetype: Petrucci X", 13),
    ("Archetype: Rabea X", 15),
    ("Archetype: Misha Mansoor X", 20),
    ("Archetype: John Mayer X", 21),
];

/// Cached file lists, plugin folders and database folders for the Directory, and a notice
/// detail per list or folder that could not be read: that part is left out and the rest of
/// the view still builds (review r11 N5, N12). Missing favorite/recent targets are skipped,
/// as stock does after a delete; so are entries whose folder key names no folder or a folder
/// without the item (`0x7d0224`, N18).
pub fn directory_view(
    library: &tetrad_library::stock::StockLibrary,
    db: Option<&StockDb>,
    media: &[MediaEntry],
    details: &[tetrad_ui::DirectoryMedia],
) -> Result<(tetrad_ui::DirectoryView, Vec<String>), super::DeviceError> {
    use tetrad_library::sidecar::Instrument;
    use tetrad_storage::media::Collection;
    use tetrad_ui::{
        DirectoryFolder, DirectoryItem, DirectoryPlugin, DirectoryView, ItemKind, MediaKind,
        PreferredInstrument,
    };
    let mut view = DirectoryView {
        media: details.to_vec(),
        ..DirectoryView::default()
    };
    let mut unread = Vec::new();
    if let Some(db) = db {
        view.pinned_captures = db.view().pinned_models()?;
        view.folders = db
            .view()
            .folders()?
            .into_iter()
            .filter_map(|folder| {
                let kind = match folder.product_type {
                    ProductType::Ir => MediaKind::Ir,
                    ProductType::Capture => MediaKind::Capture,
                    _ => return None,
                };
                Some(DirectoryFolder {
                    id: folder.id,
                    kind,
                    name: folder.name,
                    parent: folder.parent,
                    read_only: folder.read_only,
                    root: folder.folder_type == tetrad_storage::stock::FolderType::UserRoot,
                })
            })
            .collect();
    }
    let plugins = library.plugin_folders().unwrap_or_else(|error| {
        unread.push(format!("plugin folders not read: {error}"));
        Vec::new()
    });
    for folder in &plugins {
        if !folder.unreadable.is_empty() {
            unread.push(format!(
                "folders not read: {}",
                folder.unreadable.join(", ")
            ));
        }
    }
    view.plugins = plugins
        .into_iter()
        .map(|folder| DirectoryPlugin {
            plugin: (PLUGIN_FOLDERS.iter())
                .find(|(name, _)| *name == folder.name)
                .map(|&(_, plugin)| plugin),
            presets: folder
                .presets
                .into_iter()
                .take(tetrad_ui::MAX_LIST_ITEMS)
                .map(|preset| DirectoryItem {
                    legacy_index: None,
                    kind: ItemKind::Preset,
                    instrument: super::preset::preferred_instrument(Instrument::from_text(
                        &preset.instrument,
                    )),
                    folder: preset.folder,
                    value: preset.path,
                    name: preset.name,
                    author: preset.author,
                    date: preset.date,
                })
                .collect(),
            name: folder.name,
            path: folder.path,
            read_only: folder.read_only,
            children: folder.children,
        })
        .collect();
    for (kind, collection) in [
        (ItemKind::Preset, Collection::Presets),
        (ItemKind::Capture, Collection::Captures),
        (ItemKind::Ir, Collection::Irs),
    ] {
        for (list, recent) in [
            (
                tetrad_library::stock::ItemList::Favorites(collection),
                false,
            ),
            (tetrad_library::stock::ItemList::Recents(collection), true),
        ] {
            let target = if recent {
                &mut view.recents
            } else {
                &mut view.favorites
            };
            let entries = library.items(list).unwrap_or_else(|error| {
                unread.push(format!("{list:?} not read: {error}"));
                Vec::new()
            });
            if recent && kind == ItemKind::Capture {
                view.capture_recents = entries.iter().map(|entry| entry.item.clone()).collect();
            }
            for entry in entries {
                let (name, author, date, instrument) = if kind == ItemKind::Preset {
                    if !library.preset_folder_exists(&entry.folder) {
                        continue;
                    }
                    let Ok(preset) = library.load(&entry.item) else {
                        continue;
                    };
                    let sidecar = preset.sidecar.unwrap_or_default();
                    (
                        sidecar.text("name").map(str::to_owned).unwrap_or_else(|| {
                            entry
                                .item
                                .rsplit('/')
                                .next()
                                .unwrap_or(&entry.item)
                                .trim_end_matches(".pb")
                                .to_owned()
                        }),
                        sidecar.text("author").unwrap_or_default().to_owned(),
                        sidecar.text("date").unwrap_or_default().to_owned(),
                        super::preset::preferred_instrument(sidecar.instrument()),
                    )
                } else {
                    let expected = if kind == ItemKind::Capture {
                        MediaKind::Capture
                    } else {
                        MediaKind::Ir
                    };
                    if !media_folder_holds(db, &view.folders, expected, &entry) {
                        continue;
                    }
                    let Some(item) = media
                        .iter()
                        .find(|item| item.value == entry.item && item.folder.kind() == expected)
                    else {
                        continue;
                    };
                    let detail = details.iter().find(|item| item.value == entry.item);
                    (
                        item.name.clone(),
                        detail.map_or("", |item| &item.author).to_owned(),
                        detail.map_or("", |item| &item.date).to_owned(),
                        PreferredInstrument::NotSet,
                    )
                };
                target.push(DirectoryItem {
                    legacy_index: None,
                    kind,
                    folder: entry.folder,
                    value: entry.item,
                    name,
                    author,
                    date,
                    instrument,
                });
            }
        }
    }
    Ok((view, unread))
}

/// The only execution path for new Directory edits. The bring-up gate runs
/// before file reads or writes, including recent tracking.
pub fn edit_directory(
    request: tetrad_ui::DirectoryRequest,
    state: &mut super::State,
    mut media: Option<&mut MediaLibrary>,
    options: &super::bringup::RunOptions,
) -> Result<Vec<super::Effect>, super::DeviceError> {
    use super::{Effect, Subsystem};
    use tetrad_library::stock::{ItemList, ItemRef};
    use tetrad_ui::{DirectoryRequest as R, ItemKind};
    // A media recent follows an edit; without `--persist` it is skipped silently, as the
    // preset recent is (review-w6 F17).
    if options.is_bringup() && !options.persist && matches!(request, R::VisitMedia { .. }) {
        return Ok(Vec::new());
    }
    if options.is_bringup() && !options.persist && request.writes_library() {
        return Ok(vec![Effect::Notice(
            Subsystem::Platform,
            "bring-up refused a library write without --persist".into(),
        )]);
    }
    if let R::Audition { kind, folder, id } = &request {
        // Stock auditions a preset of a Downloads or Plugin folder: an ordinary load with no
        // setlist slot (`0x5b2f40`, index -1); `0x7c2974` reports it while it is current. A
        // stored DOWNLOADS row (`0x78d204` → `0x5b37f0`, r28-ui MR 4) auditions by its
        // product id: [`read_download`].
        let path = match (kind, folder.as_str()) {
            (ItemKind::Preset, DOWNLOADS_PRESETS) => format!("{DOWNLOADS_PRESETS}/{id}"),
            (ItemKind::Preset, _) if auditioned_path(id) => id.clone(),
            _ => {
                return Err(tetrad_library::Error::Unsupported(
                    "stock auditions only Downloads and Plugin presets (0x7c2974)",
                )
                .into());
            }
        };
        let probe = Effect::LoadPreset(super::PresetRef {
            setlist: None,
            index: 0,
        });
        if let Some(refused) = options.refusal(&probe) {
            return Ok(vec![Effect::Notice(
                Subsystem::Platform,
                format!("bring-up refused {refused}"),
            )]);
        }
        return Ok(vec![Effect::AuditionPreset(path)]);
    }
    let library = state
        .library
        .as_mut()
        .ok_or(super::DeviceError::Unavailable("the library"))?;
    let mut effects = Vec::new();
    let mut stale = None;
    // List writes, pushed to Control after the match.
    let mut lists: Vec<MovedItems> = Vec::new();
    match request {
        R::RenameSetlist { from, to } => {
            if from == tetrad_library::stock::DEFAULT_SETLIST {
                return Err(
                    tetrad_library::Error::Unsupported("rename the default setlist").into(),
                );
            }
            let old = library
                .presets
                .setlists()?
                .into_iter()
                .find(|s| s.name == from)
                .ok_or_else(|| tetrad_library::Error::SetlistMissing(from.clone()))?
                .path;
            stale = library.presets.rename_setlist(&from, &to)?.search_index;
            // r35-library MR 2: swap frames name the setlist, so a rename drops them (O).
            state.history.forget_setlist(&from);
            let new = library
                .presets
                .setlists()?
                .into_iter()
                .find(|s| s.name == to)
                .ok_or_else(|| tetrad_library::Error::SetlistMissing(to.clone()))?
                .path;
            if let Some(current) = state.current.get_mut().as_mut()
                && let Some(path) = &current.path
                && let Some(rest) = path.strip_prefix(&format!("{old}/"))
            {
                current.path = Some(format!("{new}/{rest}"));
            }
            if state.settings.settings().last_preset.setlist.as_deref() == Some(&old) {
                state.settings.update(|settings| {
                    settings.last_preset.setlist = Some(new);
                });
            }
        }
        R::DeleteSetlist { name } => {
            let path = library
                .presets
                .setlists()?
                .into_iter()
                .find(|s| s.name == name)
                .ok_or_else(|| tetrad_library::Error::SetlistMissing(name.clone()))?
                .path;
            stale = library
                .presets
                .delete_setlist_with_presets(&name)?
                .search_index;
            // r35-library MR 2: a setlist delete drops its swap frames (`0x76c170` → `0x647c84`).
            state.history.forget_setlist(&name);
            if let Some(current) = state.current.get_mut().as_mut()
                && current
                    .path
                    .as_ref()
                    .is_some_and(|p| p.starts_with(&format!("{path}/")))
            {
                current.path = None;
            }
            if state
                .settings
                .settings()
                .last_preset
                .setlist
                .as_deref()
                .is_some_and(|key| key == path || key == name)
            {
                let next = library
                    .presets
                    .setlists()?
                    .first()
                    .map(|setlist| setlist.path.clone());
                state.select_slot(&super::PresetRef {
                    setlist: next,
                    index: 0,
                });
            }
        }
        R::MovePreset {
            setlist,
            from,
            to,
            overwrite,
        } => {
            let mode = if overwrite {
                tetrad_library::stock::MoveMode::Overwrite
            } else {
                tetrad_library::stock::MoveMode::Swap
            };
            // r35-library MR 2: a move onto an occupied slot without OVERWRITE is a stock swap
            // (undo frame type 2, `0x648a24`).
            let swap = !overwrite
                && (library.presets.slots(&setlist)?)
                    .get(to)
                    .is_some_and(Option::is_some);
            stale = library
                .presets
                .move_slot(&setlist, from, to, mode)?
                .search_index;
            if swap {
                let slots = library.presets.slots(&setlist)?;
                let side = |slot: usize| super::preset::SwapSide {
                    setlist: setlist.clone(),
                    slot,
                    path: slots.get(slot).cloned().flatten(),
                };
                state.history.record_swap([side(from), side(to)]);
            }
            if let Some(current) = &*state.current
                && let Some(path) = &current.path
                && let Some(slot) = super::preset::locate(&library.presets, path)?
            {
                state.select_slot(&slot);
            }
        }
        R::CreateFolder { kind, parent, name } => {
            tetrad_library::stock::create_media_folder(
                library
                    .database
                    .as_mut()
                    .ok_or(super::DeviceError::Unavailable("zenui.db"))?,
                product_type(kind),
                parent,
                &name,
            )?;
        }
        R::RenameFolder { id, name } => {
            tetrad_library::stock::rename_media_folder(
                library
                    .database
                    .as_mut()
                    .ok_or(super::DeviceError::Unavailable("zenui.db"))?,
                id,
                &name,
            )?;
        }
        R::DeleteFolder {
            id,
            remove_products,
        } => {
            let db = library
                .database
                .as_mut()
                .ok_or(super::DeviceError::Unavailable("zenui.db"))?;
            let kind = db
                .view()
                .folders()?
                .into_iter()
                .find(|folder| folder.id == id)
                .map(|folder| folder.product_type)
                .ok_or(tetrad_storage::Error::FolderMissing(id))?;
            let removed = tetrad_library::stock::delete_media_folder(db, id, remove_products)?;
            let collection = if kind == ProductType::Capture {
                tetrad_storage::media::Collection::Captures
            } else {
                tetrad_storage::media::Collection::Irs
            };
            for id in removed {
                if let Some(media) = media.as_deref_mut() {
                    media.forget(&id);
                }
                library.presets.forget_media(collection, &id)?;
            }
        }
        R::RenameMedia { kind, id, name } => {
            let edit = MediaEdit {
                name: Some(name),
                ..MediaEdit::default()
            };
            lists.extend(edit_media(library, media, kind, &id, &edit)?);
        }
        R::EditCapture {
            id,
            name,
            device,
            instrument,
        } => {
            let edit = MediaEdit {
                name: Some(name),
                device: device.map(|device| device as i32),
                instrument: instrument.map(|instrument| instrument as i32),
            };
            let kind = tetrad_ui::MediaKind::Capture;
            lists.extend(edit_media(library, media, kind, &id, &edit)?);
        }
        R::DeleteMedia { kind, id } => {
            let db = library
                .database
                .as_mut()
                .ok_or(super::DeviceError::Unavailable("zenui.db"))?;
            let product = db
                .view()
                .product(&id)?
                .ok_or_else(|| tetrad_storage::Error::ProductMissing(id.clone()))?;
            if product.product_type != product_type(kind) {
                return Err(tetrad_library::Error::Unsupported("media product type").into());
            }
            db.transaction(|tx| tx.delete_product(&id))?;
            if let Some(media) = media {
                media.forget(&id);
            }
            let changed = [
                ItemList::Recents(collection(kind)),
                ItemList::Favorites(collection(kind)),
            ];
            let before = changed.map(|list| library.presets.items(list));
            library.presets.forget_media(collection(kind), &id)?;
            for (list, before) in changed.into_iter().zip(before) {
                lists.push((list, before?, None));
            }
        }
        R::FavoritePreset { path, favorite } => {
            let list = ItemList::Favorites(tetrad_storage::media::Collection::Presets);
            let entry = ItemRef {
                folder: path
                    .rsplit_once('/')
                    .map_or("", |(folder, _)| folder)
                    .to_owned(),
                item: path,
            };
            let before = library.presets.items(list)?;
            if favorite {
                library.presets.push_item(list, &entry)?;
            } else {
                library.presets.remove_item(list, &entry)?;
            }
            lists.push((list, before, favorite.then_some(entry)));
        }
        R::FavoriteMedia {
            kind,
            folder,
            id,
            favorite,
        } => {
            if favorite {
                require_media(library.database.as_ref(), kind, &id)?;
            }
            let list = ItemList::Favorites(collection(kind));
            let entry = ItemRef { folder, item: id };
            let before = library.presets.items(list)?;
            if favorite {
                library.presets.push_item(list, &entry)?;
            } else {
                for existing in before.iter().filter(|existing| existing.item == entry.item) {
                    library.presets.remove_item(list, existing)?;
                }
            }
            lists.push((list, before, favorite.then_some(entry)));
        }
        R::VisitMedia { kind, folder, id } => {
            // `0x59dd78` adds what the Captures Library root holds: a `zenui.db` product or a
            // `factory.db` capture. A key neither holds is skipped (I: a Downloads hash).
            let factory = media.as_deref().and_then(|media| media.factory.as_ref());
            let in_factory = kind == tetrad_ui::MediaKind::Capture
                && (factory
                    .map(|db| db.view().product(&id))
                    .transpose()?
                    .flatten())
                .is_some_and(|product| product.product_type == ProductType::Capture);
            let stored = (library.database.as_ref())
                .map(|db| db.view().product(&id))
                .transpose()?
                .flatten();
            match stored {
                Some(_) => require_media(library.database.as_ref(), kind, &id)?,
                None if in_factory => {}
                None => return Ok(effects),
            }
            let list = ItemList::Recents(collection(kind));
            let entry = ItemRef { folder, item: id };
            let before = library.presets.items(list)?;
            library.presets.push_item(list, &entry)?;
            lists.push((list, before, Some(entry)));
        }
        R::LoadItem {
            kind: ItemKind::Preset,
            id,
            ..
        } => {
            let target = match super::preset::locate(&library.presets, &id)? {
                Some(target) => target,
                None if id.starts_with(&format!("{}/", super::FACTORY_SETLIST)) => {
                    let index = library
                        .presets
                        .factory_slots()?
                        .iter()
                        .position(|path| path.as_deref() == Some(id.as_str()))
                        .ok_or(tetrad_library::Error::Unsupported(
                            "preset is not in the factory setlist",
                        ))?;
                    super::PresetRef {
                        setlist: Some(super::FACTORY_SETLIST.into()),
                        index: index as u32,
                    }
                }
                None => {
                    return Err(
                        tetrad_library::Error::Unsupported("preset is not in a setlist").into(),
                    );
                }
            };
            effects.push(Effect::LoadPreset(target));
        }
        // r28-device: stock has no media preview; a capture or IR favorite reveals its row
        // (tetrad-ui sends `LoadItem` only for a preset).
        R::LoadItem { .. } => {
            return Err(tetrad_library::Error::Unsupported(
                "stock has no Directory load of a capture or IR",
            )
            .into());
        }
        R::Audition { .. } => unreachable!("handled before opening the library"),
    }
    // The list signals push each device change to Cortex Control (`0x1abd00`, `0x1aabe8`).
    for (list, before, added) in lists {
        let after = (state.library.as_ref())
            .ok_or(super::DeviceError::Unavailable("the library"))?
            .presets
            .items(list)?;
        let pushes = super::control::list_pushes(state, list, &before, &after, added.as_ref());
        effects.extend(pushes);
    }
    if let Some(stale) = stale {
        effects.push(Effect::Notice(Subsystem::Library, stale.to_string()));
    }
    effects.push(Effect::Present);
    Ok(effects)
}

/// [`MediaLibrary::update`], then (`0x82ddfc`) the changed item leaves and rejoins the lists
/// that hold it (newest). Returns each changed list: (list, entries before, the moved entry).
fn edit_media(
    library: &mut super::Library,
    media: Option<&mut MediaLibrary>,
    kind: tetrad_ui::MediaKind,
    id: &str,
    edit: &MediaEdit,
) -> Result<Vec<MovedItems>, super::DeviceError> {
    use tetrad_library::stock::ItemList;
    media
        .ok_or(super::DeviceError::Unavailable("the media cipher"))?
        .update(
            (library.database.as_mut()).ok_or(super::DeviceError::Unavailable("zenui.db"))?,
            kind,
            id,
            edit,
        )?;
    let mut lists = Vec::new();
    for list in [
        ItemList::Recents(collection(kind)),
        ItemList::Favorites(collection(kind)),
    ] {
        let before = library.presets.items(list)?;
        let mut moved = None;
        for entry in before.iter().rev().filter(|entry| entry.item == id) {
            library.presets.push_item(list, entry)?;
            moved = Some(entry.clone());
        }
        if moved.is_some() {
            lists.push((list, before, moved));
        }
    }
    Ok(lists)
}

/// A list write of [`edit_directory`]: (list, entries before, the added entry).
type MovedItems = (
    tetrad_library::stock::ItemList,
    Vec<tetrad_library::stock::ItemRef>,
    Option<tetrad_library::stock::ItemRef>,
);

/// A preset below a Plugins root (`LocalPluginPresetFolder`), or a stored Downloads preset
/// ([`read_download`]). While one is current the preset is auditioned (`0x7c2974`): stock
/// ignores MIDI PC (`0x5b5bd8`) and records no preset recent (`0x7c3820` needs a setlist
/// folder).
pub fn auditioned_path(path: &str) -> bool {
    ["/media/p4/Plugins/", "/opt/neuraldsp/Plugins/"]
        .iter()
        .any(|root| path.starts_with(root))
        || path.starts_with(DOWNLOADS_PRESETS_PREFIX)
}

/// The DOWNLOADS presets folder key of a Directory request (`cloud-<type>-<scope>`: preset
/// type 0, Downloads 1).
pub const DOWNLOADS_PRESETS: &str = "cloud-0-1";

/// The current-preset path of an auditioned stored Downloads preset is this prefix and its
/// product id. It names no file, so a save refuses it (not a `.pb` path).
const DOWNLOADS_PRESETS_PREFIX: &str = "cloud-0-1/";

/// The preset of a stored Downloads preset audition (`cloud-0-1/<product id>`; `0x78d204` →
/// `0x5b37f0`, index -1): its `downloads_product_preset` row (`load_download`). `None`:
/// `path` names no Downloads preset. Preset ± during it walks the stored DOWNLOADS presets
/// ([`audition_step`]).
pub fn read_download(
    state: &super::State,
    path: &str,
) -> Option<Result<tetrad_preset::Preset, super::DeviceError>> {
    let id = path.strip_prefix(DOWNLOADS_PRESETS_PREFIX)?;
    let read = || {
        let db = (state.library.as_ref())
            .and_then(|library| library.database.as_ref())
            .ok_or(super::DeviceError::Unavailable("the library database"))?;
        let limits = tetrad_preset::Limits::default();
        let (document, _) = tetrad_library::stock::load_download(&db.view(), id, limits)?
            .ok_or_else(|| tetrad_library::Error::PresetMissing(path.to_owned()))?;
        let preset =
            tetrad_preset::Preset::try_from(&document).map_err(super::preset::PresetError::from)?;
        Ok(preset)
    };
    Some(read())
}

/// Preset ± while a plugin preset is auditioned (`0x5b57b8` → `0x780e1c`): the neighbour in
/// its folder, wrapping at both ends, is auditioned again (index -1, through the Directory
/// gate). `None`: no audition, the step walks the setlist. Inferred: the folder order is the
/// library listing; stock walks the Directory list model, which follows the user sort. A
/// stored DOWNLOADS preset (r33-ui MR D) walks the stored presets in table order, the order
/// of their rows in [`super::Device::downloads_rows`] (I: queue rows not stored are skipped).
pub fn audition_step(state: &super::State, step: i8) -> Option<Vec<super::Effect>> {
    use super::{Effect, Subsystem};
    let path = state.current.as_ref()?.path.as_deref()?;
    if !auditioned_path(path) {
        return None;
    }
    let Some(library) = state.library.as_ref() else {
        return Some(Vec::new());
    };
    let neighbour = |ids: &[String], at: usize| {
        let next = (at as isize + if step < 0 { -1 } else { 1 }).rem_euclid(ids.len() as isize);
        ids[next as usize].clone()
    };
    if let Some(id) = path.strip_prefix(DOWNLOADS_PRESETS_PREFIX) {
        let Some(db) = library.database.as_ref() else {
            return Some(Vec::new());
        };
        let ids = match db.view().download_ids(DownloadKind::Preset) {
            Ok(ids) => ids,
            Err(error) => return Some(vec![Effect::Notice(Subsystem::Library, error.to_string())]),
        };
        let at = ids.iter().position(|other| other == id);
        return Some(
            (at.map(|at| neighbour(&ids, at)).into_iter())
                .map(|id| {
                    Effect::Directory(tetrad_ui::DirectoryRequest::Audition {
                        kind: tetrad_ui::ItemKind::Preset,
                        folder: DOWNLOADS_PRESETS.to_owned(),
                        id,
                    })
                })
                .collect(),
        );
    }
    let folders = match library.presets.plugin_folders() {
        Ok(folders) => folders,
        Err(error) => return Some(vec![Effect::Notice(Subsystem::Library, error.to_string())]),
    };
    let next = folders.into_iter().find_map(|plugin| {
        let at = plugin
            .presets
            .iter()
            .position(|preset| preset.path == path)?;
        let folder = &plugin.presets[at].folder;
        let siblings: Vec<_> = plugin
            .presets
            .iter()
            .filter(|preset| &preset.folder == folder)
            .collect();
        let at = siblings.iter().position(|preset| preset.path == path)?;
        let paths: Vec<String> = siblings.iter().map(|preset| preset.path.clone()).collect();
        Some((plugin.path.clone(), neighbour(&paths, at)))
    });
    Some(
        next.map(|(folder, id)| {
            Effect::Directory(tetrad_ui::DirectoryRequest::Audition {
                kind: tetrad_ui::ItemKind::Preset,
                folder,
                id,
            })
        })
        .into_iter()
        .collect(),
    )
}

/// Media recents for the string values of a block that was added, placed, changed or
/// pasted (`0x59dd78`). Stock reads a capture block's value at scene 0 and IR values at the
/// current scene; owned: every value at `scene`, else its first stored scene.
pub fn recent_block_media(
    db: Option<&StockDb>,
    block: &tetrad_engine::Instance,
    scene: tetrad_engine::SceneIndex,
) -> Vec<super::Effect> {
    block
        .texts
        .iter()
        .filter_map(|text| {
            text.scenes
                .get(scene.index())
                .and_then(Option::as_deref)
                .or_else(|| text.scenes.iter().flatten().next().map(String::as_str))
        })
        .filter_map(|value| match recent_media_selection(db, value) {
            Ok(request) => request.map(super::Effect::Directory),
            Err(error) => Some(super::Effect::Notice(
                super::Subsystem::Library,
                error.to_string(),
            )),
        })
        .collect()
}

/// A media recent for a block string value that names a library product. Stock
/// (`0x59dd78`) runs on a block parameter change (`0x1727a0`), a block add (`0x4b7aa4`),
/// a model change (`0x4ba280`) and a placement (`0x59ec78`, `0x59ee7c`), never on a
/// preset load. Captures need a 64-unit key; the item is filed under its library root.
pub fn recent_media_selection(
    db: Option<&StockDb>,
    value: &str,
) -> tetrad_storage::Result<Option<tetrad_ui::DirectoryRequest>> {
    let Some(db) = db else {
        return Ok(None);
    };
    let id = capture_key(value);
    let Some(product) = db.view().product(id)? else {
        // A key `zenui.db` does not hold may be a `factory.db` capture; `edit_directory`
        // looks there and skips a key no store holds.
        let key = id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit());
        return Ok(key.then(|| tetrad_ui::DirectoryRequest::VisitMedia {
            kind: tetrad_ui::MediaKind::Capture,
            folder: "local_nc_root".into(),
            id: id.into(),
        }));
    };
    let (kind, folder) = match product.product_type {
        ProductType::Capture => (tetrad_ui::MediaKind::Capture, "local_nc_root"),
        ProductType::Ir => (tetrad_ui::MediaKind::Ir, "local_ir_root"),
        _ => return Ok(None),
    };
    Ok(Some(tetrad_ui::DirectoryRequest::VisitMedia {
        kind,
        folder: folder.into(),
        id: id.into(),
    }))
}

fn require_media(
    db: Option<&StockDb>,
    kind: tetrad_ui::MediaKind,
    id: &str,
) -> Result<(), super::DeviceError> {
    let db = db.ok_or(super::DeviceError::Unavailable("zenui.db"))?;
    let product = db
        .view()
        .product(id)?
        .ok_or_else(|| tetrad_storage::Error::ProductMissing(id.to_owned()))?;
    if product.product_type != product_type(kind) {
        return Err(tetrad_library::Error::Unsupported("media product type").into());
    }
    Ok(())
}

/// Folder menu "SET AS SAVING DESTINATION" (manual M821): the folder's stock key into
/// `downloadDestinationFolderIR` / `NC` (`0x64ee28`). Read back with
/// `tetrad_library::stock::saving_destination`, which falls back to the user root.
pub fn set_saving_destination(
    state: &mut super::State,
    folder: i64,
) -> Result<Vec<super::Effect>, super::actions::ActionError> {
    use super::actions::ActionError;
    let found = state
        .library
        .as_ref()
        .and_then(|library| library.database.as_ref())
        .ok_or(ActionError::Unavailable("zenui.db"))?
        .view()
        .folders()?
        .into_iter()
        .find(|candidate| candidate.id == folder)
        .ok_or(tetrad_storage::Error::FolderMissing(folder))?;
    if found.read_only {
        return Err(tetrad_storage::Error::ReadOnly.into());
    }
    let key = tetrad_library::stock::media_folder_key(&found).ok_or(ActionError::Unsupported(
        "a saving destination must be a capture or IR folder",
    ))?;
    let kind = found.product_type;
    Ok(super::actions::update_settings(state, |settings| {
        let library = &mut settings.library;
        match kind {
            ProductType::Ir => library.download_destination_ir = Some(key),
            _ => library.download_destination_nc = Some(key),
        }
    }))
}

/// Boot, before the library opens: the stock `zenui.db` patch chain (`0x00483b18`). It runs
/// only when the run may persist and the database does not open as 410.1. The text of a
/// notice comes back when it changed something or stopped; a stopped upgrade keeps the file
/// and the earlier patches (`StockDb::upgrade` rolls the failed patch back).
pub fn upgrade_database(
    path: &std::path::Path,
    options: &super::bringup::RunOptions,
) -> Option<String> {
    use tetrad_storage::stock::{COROS_4_1_SCHEMA, DbLimits};
    if options
        .boot_refusal(super::bringup::BootDataStep::DatabaseUpgrade)
        .is_some()
        || !path.exists()
        || StockDb::open(path, DbLimits::default(), true)
            .is_ok_and(|db| db.version() >= COROS_4_1_SCHEMA)
    {
        return None;
    }
    match StockDb::upgrade(path, DbLimits::default()) {
        Ok(applied) if applied.is_empty() => None,
        Ok(applied) => Some(format!(
            "zenui.db upgraded with the stock patches {}",
            applied
                .iter()
                .map(|version| format!("{}.{}", version.major, version.minor))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Err(error) => Some(format!(
            "zenui.db upgrade stopped, the file is kept: {error}"
        )),
    }
}

impl<P: super::platform::Platform> super::Device<P> {
    /// Boot, after the catalog and the IR library: both capture stores to local (`0x6a6358`,
    /// `0x6a68f4`), then the legacy IR and capture migrations (`0x6e72d4`, `0x6e71cc`). Stock
    /// order (review r18 F2): the capture store (`0x7044b8` → `0x6a6f78`) comes before the
    /// legacy steps, so legacy captures, stored as they are, never get a cipher pass. Each
    /// step is refused in a bring-up run, is idempotent (a translated store is flagged; a
    /// migrated directory is renamed; a failed step leaves its store or directory for the
    /// next boot) and logs one notice when it changed something or failed.
    pub(super) fn migrate_boot_media(&mut self) {
        use super::bringup::BootDataStep as Step;
        // The store translation re-keys the Downloads capture rows (review r29 B10).
        self.bridges.services.downloads_changed();
        let allowed = |step| self.options.boot_refusal(step).is_none();
        let (irs, captures, translation) = (
            allowed(Step::LegacyIrs),
            allowed(Step::LegacyCaptures),
            allowed(Step::CaptureStoreTranslation),
        );
        if self
            .options
            .boot_refusal(Step::LegacyCaptureFiles)
            .is_none()
        {
            self.migrate_zen_rs_files();
        }
        // `0x704198`: the legacy capture files before the stores, their conversion after. The
        // legacy import stores the files as they are, so it waits for their translation (O).
        let translating = self.translate_legacy_capture_files();
        let captures = captures && !translating;
        // The store translation and the IR metadata need the device cipher; the legacy
        // captures are stored as they are (`0x6e6c18`).
        let Some(local) = self.media.as_ref().map(|media| media.cipher().clone()) else {
            let converting = self.convert_legacy_captures(translating);
            self.migrate_legacy_media(None, false, captures && !converting);
            return;
        };
        let pending = translation
            && self
                .state
                .library
                .as_ref()
                .and_then(|library| library.database.as_ref())
                .is_some_and(|db| {
                    untranslated_stores(&db.view()).is_ok_and(|stores| !stores.is_empty())
                });
        // The seed-only cipher is needed only when a store has rows to re-encrypt.
        let global = if pending {
            self.capture_ciphers().map(|(global, _)| global)
        } else {
            Ok(local.clone())
        };
        if translation
            && let Some(super::Library {
                database: Some(db), ..
            }) = self.state.library.as_mut()
        {
            let notice = match global
                .map(|global| tetrad_library::stock::translate_global_stores(db, &global, &local))
            {
                Ok(Ok(outcomes)) => {
                    let translated: Vec<_> = [CaptureStore::Downloads, CaptureStore::User]
                        .into_iter()
                        .zip(outcomes)
                        .filter_map(|(store, outcome)| match outcome {
                            Some(tetrad_storage::stock::StoreTranslation::Translated(count)) => {
                                Some(format!("{store:?} {count}"))
                            }
                            _ => None,
                        })
                        .collect();
                    (!translated.is_empty()).then(|| {
                        format!(
                            "capture stores re-encrypted for this device and flagged local: {}",
                            translated.join(", ")
                        )
                    })
                }
                Ok(Err(error)) => Some(format!("capture stores kept as they are: {error}")),
                Err(error) => Some(format!(
                    "capture stores kept as they are: no ZenUI seed: {error}"
                )),
            };
            if let Some(notice) = notice {
                self.notice(super::Subsystem::Library, notice);
            }
        }
        let converting = self.convert_legacy_captures(translating);
        self.migrate_legacy_media(Some(&local), irs, captures && !converting);
    }

    /// The zen-rs to Tetrad rename of the staged legacy capture copies, before the steps that
    /// commit them (`StockLibrary::migrate_zen_rs_files`). One notice when an old copy stays
    /// beside its Tetrad copy, or when the step fails.
    fn migrate_zen_rs_files(&mut self) {
        let Some(library) = self.state.library.as_ref() else {
            return;
        };
        let detail = match library.presets.migrate_zen_rs_files() {
            Ok(kept) if kept.is_empty() => return,
            Ok(kept) => format!(
                "zen-rs capture copies kept beside their Tetrad copies: {}",
                kept.join(", ")
            ),
            Err(error) => format!("zen-rs capture copies not renamed: {error}"),
        };
        self.notice(super::Subsystem::Library, detail);
    }

    /// `0x703900`, at boot while [`CAPTURE_TRANSLATION_FLAG`] exists (the restore reload sets
    /// it): each legacy capture whose name the flag file does not hold yet is re-keyed from
    /// the seed-only to the device cipher and its name joins the flag file (one per line,
    /// `0x69ab98`, `0x69aad8`), so a cut run resumes. Then the flag goes. O (review r23 K4):
    /// the re-keyed copies are staged, the flag file is replaced atomically, and only then
    /// do the copies replace the files; stock re-keys in place and truncates the flag file.
    /// Returns whether the step is still pending (no flag: not pending).
    fn translate_legacy_capture_files(&mut self) -> bool {
        let flag = Path::new(CAPTURE_TRANSLATION_FLAG);
        let done = match self.platform.services().files().read(flag) {
            Ok(None) => return false,
            Ok(Some(done)) => done,
            Err(error) => {
                let detail = format!("legacy capture translation: {error}");
                self.notice(super::Subsystem::Library, detail);
                return true;
            }
        };
        let step = super::bringup::BootDataStep::LegacyCaptureFiles;
        if self.options.boot_refusal(step).is_some() {
            return true;
        }
        let (global, local) = match self.capture_ciphers() {
            Ok(ciphers) => ciphers,
            Err(error) => {
                let detail = format!("legacy captures wait for their translation: {error}");
                self.notice(super::Subsystem::Library, detail);
                return true;
            }
        };
        let Some(library) = self.state.library.as_ref() else {
            return true;
        };
        let mut done: BTreeSet<String> = String::from_utf8_lossy(&done)
            .lines()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        let files = self.platform.services().files();
        let presets = &library.presets;
        let mut translate = || -> Result<usize, String> {
            // A journaled capture is re-keyed: a cut run may have left its copies uncommitted.
            for name in &done {
                presets
                    .commit_legacy_capture(name)
                    .map_err(|e| e.to_string())?;
            }
            let names = presets.legacy_capture_names().map_err(|e| e.to_string())?;
            let mut translated = 0;
            let pending: Vec<_> = names
                .into_iter()
                .filter(|name| !done.contains(name))
                .collect();
            // Review r23 K4: stage, journal (atomic), then commit, so a cut at any point
            // never re-keys a file twice.
            for name in pending {
                (presets.stage_legacy_capture(&name, &global, &local))
                    .map_err(|e| e.to_string())?;
                done.insert(name.clone());
                let journal: String = done.iter().map(|name| format!("{name}\n")).collect();
                files
                    .write_atomic(flag, journal.as_bytes())
                    .map_err(|e| e.to_string())?;
                presets
                    .commit_legacy_capture(&name)
                    .map_err(|e| e.to_string())?;
                translated += 1;
            }
            files.remove(flag).map_err(|e| e.to_string())?;
            Ok(translated)
        };
        let result = translate();
        let (pending, detail) = match result {
            Ok(0) => (false, None),
            Ok(count) => (
                false,
                Some(format!(
                    "legacy captures moved to the device cipher: {count}"
                )),
            ),
            Err(error) => (
                true,
                Some(format!(
                    "legacy capture translation stopped, resumes at the next boot: {error}"
                )),
            ),
        };
        if let Some(detail) = detail {
            self.notice(super::Subsystem::Library, detail);
        }
        pending
    }

    /// The legacy format conversion, at boot while [`CAPTURE_CONVERSION_FINISHED`] is missing
    /// (`0x704198` → `0x702d58`, `0x6f77f8`, `0x702064`): each legacy capture without a sidecar
    /// becomes `<hash>.cns` and `<hash>.json` (`StockLibrary::stage_legacy_conversion`). Stock
    /// deletes a capture that does not convert; Tetrad keeps it and names it (O). O: stage,
    /// journal ([`CAPTURE_CONVERSION_JOURNAL`], atomic), then commit; the journaled captures
    /// commit first, so a cut run never converts a file twice, and the old file goes only after
    /// its converted pair reads back. Names a cut stock run kept in [`CONVERSION_DELETED_NAMES`]
    /// (`0x702e24`: a JSON array of paths, shown by base name) join the Corrupt Captures list.
    /// Then the marker is written and the name cache removed (`0x702064`). Returns whether the
    /// step is still pending: with work while the translation is pending, without the device
    /// cipher or the library, or after an error. The legacy capture import waits with it, as
    /// it would move the unconverted files out of "My Captures" (O).
    fn convert_legacy_captures(&mut self, translation_pending: bool) -> bool {
        use tetrad_library::stock::{LegacyCommit, LegacyConversion};
        let step = super::bringup::BootDataStep::LegacyCaptureFiles;
        if self.options.boot_refusal(step).is_some() {
            return true;
        }
        let files = self.platform.services().files();
        let finished = Path::new(CAPTURE_CONVERSION_FINISHED);
        if !matches!(files.read(finished), Ok(None)) {
            return false;
        }
        let Some(library) = self.state.library.as_ref() else {
            return true;
        };
        let presets = &library.presets;
        let local = self.media.as_ref().map(MediaLibrary::cipher);
        let journal_path = Path::new(CAPTURE_CONVERSION_JOURNAL);
        let mut convert = || -> Result<Option<(usize, Vec<String>)>, String> {
            let mut journal: BTreeMap<String, String> = match files.read(journal_path) {
                Ok(Some(bytes)) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
                Ok(None) => BTreeMap::new(),
                Err(error) => return Err(error.to_string()),
            };
            let unconverted = presets
                .unconverted_legacy_captures()
                .map_err(|e| e.to_string())?;
            if journal.is_empty() && unconverted.is_empty() {
                return Ok(Some((0, Vec::new())));
            }
            if translation_pending {
                return Ok(None);
            }
            let local = local.ok_or("no device cipher")?;
            let mut converted = 0;
            let mut kept = Vec::new();
            // A journaled capture is converted: a cut run may have left it uncommitted. An
            // entry with no old file and no pair left is dropped (`NotStaged`).
            for (file, hash) in std::mem::take(&mut journal) {
                let commit = presets.commit_legacy_conversion(&file, &hash, local);
                if let LegacyCommit::Kept(reason) = commit.map_err(|e| e.to_string())? {
                    kept.push(format!("{file} ({reason})"));
                }
            }
            // Review r25 Q1: a capture that does not convert is named and the rest go on.
            for file in presets
                .unconverted_legacy_captures()
                .map_err(|e| e.to_string())?
            {
                match (presets.stage_legacy_conversion(&file, local)).map_err(|e| e.to_string())? {
                    LegacyConversion::Kept(reason) => kept.push(format!("{file} ({reason})")),
                    LegacyConversion::Staged { hash } => {
                        journal.clear();
                        journal.insert(file.clone(), hash.clone());
                        let line = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
                        (files.write_atomic(journal_path, &line)).map_err(|e| e.to_string())?;
                        match (presets.commit_legacy_conversion(&file, &hash, local))
                            .map_err(|e| e.to_string())?
                        {
                            LegacyCommit::Committed => converted += 1,
                            LegacyCommit::Kept(reason) => kept.push(format!("{file} ({reason})")),
                            LegacyCommit::NotStaged => kept.push(format!("{file} (not staged)")),
                        }
                    }
                }
            }
            files.remove(journal_path).map_err(|e| e.to_string())?;
            Ok(Some((converted, kept)))
        };
        let (kept, detail) = match convert() {
            Ok(Some((converted, kept))) => (
                kept,
                (converted > 0).then(|| {
                    format!("legacy captures converted to the current format: {converted}")
                }),
            ),
            Ok(None) => return true,
            Err(error) => {
                let detail =
                    format!("legacy capture conversion stopped, resumes at the next boot: {error}");
                self.notice(super::Subsystem::Library, detail);
                return true;
            }
        };
        if let Some(detail) = detail {
            self.notice(super::Subsystem::Library, detail);
        }
        let files = self.platform.services().files();
        let cache = Path::new(CONVERSION_DELETED_NAMES);
        let deleted: Vec<String> = files
            .read(cache)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_slice::<Vec<String>>(&json).ok())
            .unwrap_or_default()
            .iter()
            .map(|path| {
                let file = path.rsplit('/').next().unwrap_or(path);
                file.split('.').next().unwrap_or(file).to_owned()
            })
            .collect();
        let marked = files
            .write(finished, b"")
            .and_then(|()| files.remove(cache))
            .and_then(|()| files.sync());
        self.bridges.services.corrupt_captures.extend(deleted);
        let detail = match marked {
            Err(error) => Some(format!("legacy capture conversion marker: {error}")),
            Ok(()) if !kept.is_empty() => Some(format!(
                "legacy captures that do not convert are kept, not deleted: {}",
                kept.join(", ")
            )),
            Ok(()) => None,
        };
        if let Some(detail) = detail {
            self.notice(super::Subsystem::Library, detail);
        }
        false
    }

    /// `0x702f70`, at boot while [`CAPTURE_CORRUPTION_CHECKED`] is missing (the restore reload
    /// removes it). Call it after the NC2 key (DSP vector `0x27`). Each Downloads or user
    /// capture whose metadata or rawdata does not load leaves the library for the quarantine
    /// table (O: stock deletes it; `tetrad_library::stock::restore_quarantined_capture` puts it
    /// back) and its name joins the Corrupt Captures list. When too many fail, none moves
    /// (`quarantine_corrupt_captures`, review r23 K5). The marker is written only when every
    /// row was checked and none was held. The check runs only when each capture store with
    /// rows is flagged local, as a store in another cipher would read as corrupt (K1).
    pub(super) fn check_capture_corruption(&mut self) {
        use tetrad_storage::stock::QUARANTINE_TABLE;
        let step = super::bringup::BootDataStep::CaptureCorruptionCheck;
        let marker = Path::new(CAPTURE_CORRUPTION_CHECKED);
        if self.options.boot_refusal(step).is_some()
            || !matches!(self.platform.services().files().read(marker), Ok(None))
        {
            return;
        }
        let (
            Some(media),
            Some(super::Library {
                database: Some(db), ..
            }),
        ) = (self.media.as_ref(), self.state.library.as_mut())
        else {
            return;
        };
        // Review r23 K1: a global store with rows is not translated yet (no seed, or a
        // failed transaction), so it would read as corrupt; stock always translates first.
        let waits = match untranslated_stores(&db.view()) {
            Ok(stores) if stores.is_empty() => None,
            Ok(stores) => Some(format!(
                "{} not re-encrypted for this device yet",
                (stores.iter())
                    .map(|(store, flag)| format!("{store:?} capture store {flag:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Err(error) => Some(error.to_string()),
        };
        if let Some(error) = waits {
            let detail = format!("capture corruption check waits: {error}");
            self.notice(super::Subsystem::Library, detail);
            return;
        }
        let check = tetrad_library::stock::quarantine_corrupt_captures(db, |raw, metadata| {
            media.check_capture(raw, metadata)
        });
        // A quarantined capture may leave the Downloads table (review r29 B10).
        self.bridges.services.downloads_changed();
        let check = match check {
            Ok(check) => check,
            Err(error) => {
                let detail = format!("capture corruption check failed, nothing removed: {error}");
                self.notice(super::Subsystem::Library, detail);
                return;
            }
        };
        let mut details = Vec::new();
        if !check.held.is_empty() {
            details.push(format!(
                "capture corruption check moved nothing: {} captures do not read, too many for \
                 corruption, so a Tetrad reader or cipher fault is more likely; it runs again at \
                 the next boot: {}",
                check.held.len(),
                (check.held.iter())
                    .map(|row| format!("{} ({})", row.name, row.reason))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !check.removed.is_empty() {
            details.push(format!(
                "corrupt captures moved out of the library into {QUARANTINE_TABLE}: {}",
                (check.removed.iter())
                    .map(|row| format!("{} ({})", row.name, row.reason))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if check.unchecked.is_empty() && check.held.is_empty() {
            let files = self.platform.services().files();
            if let Err(error) = files.write(marker, b"").and_then(|()| files.sync()) {
                details.push(format!("capture corruption check marker: {error}"));
            }
        } else if !check.unchecked.is_empty() {
            details.push(format!(
                "captures not checked, the check runs again at the next boot: {}",
                check.unchecked.join(", ")
            ));
        }
        let names = check.removed.into_iter().map(|row| row.name);
        self.bridges.services.corrupt_captures.extend(names);
        for detail in details {
            self.notice(super::Subsystem::Library, detail);
        }
    }

    /// The names for the "Corrupt Neural Capture" dialog (`0x54663c`) that boot or a restore
    /// found since the last call, in stock order (conversion, then the corruption check). The
    /// UI joins them with ", " (`0xa10c40`) into its `%1`. Empty: no dialog.
    pub fn take_corrupt_captures(&mut self) -> Vec<String> {
        std::mem::take(&mut self.bridges.services.corrupt_captures)
    }

    /// The legacy IR (needs the device cipher) and capture migrations of
    /// [`Self::migrate_boot_media`].
    fn migrate_legacy_media(&mut self, local: Option<&DeviceCipher>, irs: bool, captures: bool) {
        let Some(super::Library {
            presets,
            database: Some(db),
        }) = self.state.library.as_mut()
        else {
            return;
        };
        let mut notices = Vec::new();
        if irs && let (Some(local), Some(specs)) = (local, self.state.specs.as_ref()) {
            let model = |id| {
                specs.model(id).map(|model| {
                    let base = model.clones.first().copied().unwrap_or(model.id);
                    (model.category_id, base)
                })
            };
            match presets.migrate_legacy_irs(db, local, tetrad_firmware::running().version, model) {
                Ok(Some(report)) => notices.push(format!(
                    "legacy IRs: {} moved into zenui.db, {} presets updated, {} files left in \
                     the _legacy directory",
                    report.products.len(),
                    report.presets.len(),
                    report.skipped.len()
                )),
                Ok(None) => {}
                Err(error) => notices.push(format!("legacy IRs kept for the next boot: {error}")),
            }
        }
        if captures {
            match presets.migrate_legacy_captures(db) {
                Ok(Some(report)) => notices.push(format!(
                    "legacy captures: {} moved into zenui.db, {} without metadata left in the \
                     _legacy directory",
                    report.products.len(),
                    report.skipped.len()
                )),
                Ok(None) => {}
                Err(error) => {
                    notices.push(format!("legacy captures kept for the next boot: {error}"))
                }
            }
        }
        for notice in notices {
            self.notice(super::Subsystem::Library, notice);
        }
    }
}
