//! Library side of user IRs and captures in `zenui.db`: stock folder keys and
//! the saving destination, storing a product into a folder, the Control/cloud
//! IR import, the two legacy media migrations and the capture store
//! translation.

use std::collections::BTreeMap;
use std::fs;
use std::time::SystemTime;

use tetrad_media::capture_store::translate_capture;
use tetrad_media::factory::DeviceCipher;
use tetrad_media::ir::{IrInputFormat, IrLimits, MAX_IR_SAMPLES, WavPolicy, decode};
use tetrad_media::user::{
    LEGACY_USER_IR_DIR, UserIrMetadata, UserIrProduct, product_id, reference_params,
};
use tetrad_preset::{COLUMNS, ParamValue, Preset, ROWS, SCENES, Slot};
use tetrad_storage::media::read_file;
use tetrad_storage::stock::{
    CaptureEncryption, CaptureStore, FolderType, NewFolder, ProductType, StockDb, StockFolder,
    StockProduct, StockView, StockWrite, StoreTranslation, USER_ROOTS,
};

use super::StockLibrary;
use super::directory::named_entries;
use crate::sidecar::utc_date;
use crate::{Error, Result};

/// Items per media library (manual: "up to 2048 Neural Captures", "up to 2048
/// IRs"; the stock check site was not found).
pub const MEDIA_LIBRARY_CAP: usize = 2048;
/// Folder keys of the user roots (`favorites-ir`, `lastTableFolder`,
/// `downloadDestinationFolder*`). A user folder key is `<id>_q`.
pub const IR_ROOT_KEY: &str = "local_ir_root";
pub const CAPTURE_ROOT_KEY: &str = "local_nc_root";
const USER_FOLDER_SUFFIX: &str = "_q";
/// Pre-4.x capture files (`0x006e71cc`).
pub const LEGACY_CAPTURE_DIR: &str = "/media/p4/Captures/My Captures";
/// A migrated legacy directory is renamed to `<dir>_legacy` (`0x006e45dc`).
pub const LEGACY_SUFFIX: &str = "_legacy";

/// The stock key of a user media folder; `None` for factory and preset folders.
pub fn media_folder_key(folder: &StockFolder) -> Option<String> {
    match (folder.folder_type, folder.product_type) {
        (FolderType::UserRoot, ProductType::Ir) => Some(IR_ROOT_KEY.to_owned()),
        (FolderType::UserRoot, ProductType::Capture) => Some(CAPTURE_ROOT_KEY.to_owned()),
        (FolderType::User, ProductType::Ir | ProductType::Capture) => {
            Some(format!("{}{USER_FOLDER_SUFFIX}", folder.id))
        }
        _ => None,
    }
}

/// The writable folder of `kind` that `key` names, if any.
pub fn media_folder_by_key(
    view: &StockView<'_>,
    kind: ProductType,
    key: &str,
) -> Result<Option<i64>> {
    Ok(view.folders()?.into_iter().find_map(|folder| {
        (folder.product_type == kind
            && !folder.read_only
            && media_folder_key(&folder).as_deref() == Some(key))
        .then_some(folder.id)
    }))
}

/// The folder new items of `kind` go to: the stored `downloadDestinationFolderNC`
/// / `IR` key when it names a writable folder of that kind, else the user root
/// (`0x0064ba04`, default folder `0x76be70`). Stock "SET AS SAVING
/// DESTINATION" stores [`media_folder_key`] of the chosen folder.
pub fn saving_destination(
    view: &StockView<'_>,
    kind: ProductType,
    stored: Option<&str>,
) -> Result<i64> {
    if let Some(key) = stored
        && let Some(id) = media_folder_by_key(view, kind, key)?
    {
        return Ok(id);
    }
    view.root_folder(kind)?
        .ok_or_else(|| Error::Unsupported("the media user root is missing; run boot maintenance"))
}

/// Where [`store_media`] put a product.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMedia {
    pub id: String,
    /// The id existed and its row was replaced (Control import "update", vtable
    /// `+0x50`); otherwise it was added (`+0x54`).
    pub replaced: bool,
}

/// Puts a user IR or capture row and links it to its user root and to
/// `folder` (a writable folder of the same type), in one transaction. A new id
/// needs room under [`MEDIA_LIBRARY_CAP`]. Linking the root as well as a
/// subfolder is inferred from the stock capture save and legacy migrations.
pub fn store_media(db: &mut StockDb, product: &StockProduct, folder: i64) -> Result<StoredMedia> {
    Ok(db.transaction(|write| store_in(write, product, folder))?)
}

pub(super) fn store_in(
    write: &StockWrite<'_>,
    product: &StockProduct,
    folder: i64,
) -> tetrad_storage::Result<StoredMedia> {
    let kind = product.product_type;
    let target = write
        .folders()?
        .into_iter()
        .find(|candidate| candidate.id == folder)
        .ok_or(tetrad_storage::Error::FolderMissing(folder))?;
    if !matches!(kind, ProductType::Ir | ProductType::Capture)
        || target.product_type != kind
        || target.read_only
    {
        return Err(tetrad_storage::Error::ReadOnly);
    }
    let replaced = write.product(&product.id)?.is_some();
    if !replaced && write.product_ids(kind)?.len() >= MEDIA_LIBRARY_CAP {
        return Err(tetrad_storage::Error::Limit);
    }
    let root = user_root(write, kind)?;
    write.put_product(product)?;
    write.link(root, &product.id)?;
    if folder != root {
        write.link(folder, &product.id)?;
    }
    Ok(StoredMedia {
        id: product.id.clone(),
        replaced,
    })
}

/// Get-or-create of the user root, as boot does (`0x00834e10`).
pub(super) fn user_root(write: &StockWrite<'_>, kind: ProductType) -> tetrad_storage::Result<i64> {
    let (_, name) = USER_ROOTS
        .into_iter()
        .find(|(root_kind, _)| *root_kind == kind)
        .ok_or(tetrad_storage::Error::Identity)?;
    write.ensure_folder(&NewFolder {
        product_type: kind,
        folder_type: FolderType::UserRoot,
        name,
        parent: 0,
        read_only: false,
    })
}

/// One WAV to import as a user IR (Cortex Control `0x0024dae0`, cloud
/// download `0x0072884c`).
#[derive(Debug, Clone, Copy)]
pub struct IrImport<'a> {
    pub wav: &'a [u8],
    /// Required: Control refuses a file message without its name field.
    pub name: &'a str,
    pub date: SystemTime,
    pub cloud_id: &'a str,
    pub coros_version: &'a str,
}

/// Imports a WAV into `folder` (a writable IR folder; Control names it by key,
/// `0x76bc68`). Stock rule: a non-empty payload, the recovered extractor
/// (`0x69c384`: RIFF/WAVE, format 3, mono, 48,000 Hz, 32 bits) and the first
/// 1,024 samples. There is no resampler or downmix on the device; any other
/// format is refused. The id is `CIR_` + MD5 of the selected words
/// (`0x829468`); an existing id is replaced. Metadata is encrypted with
/// `cipher` (mode 1, `0x6a974c`).
pub fn import_user_ir(
    db: &mut StockDb,
    folder: i64,
    import: &IrImport<'_>,
    cipher: &DeviceCipher,
    limits: IrLimits,
) -> Result<StoredMedia> {
    let metadata = UserIrMetadata {
        name: import.name.to_owned(),
        date: utc_date(import.date),
        cloud_id: import.cloud_id.to_owned(),
        coros_version: import.coros_version.to_owned(),
    };
    let product = ir_product(import.wav, metadata, cipher, limits)?;
    store_media(db, &product, folder)
}

/// The `zenui.db` row of a WAV IR: the extractor, the id and the encrypted metadata.
fn ir_product(
    wav: &[u8],
    metadata: UserIrMetadata,
    cipher: &DeviceCipher,
    limits: IrLimits,
) -> Result<StockProduct> {
    // An empty payload fails in the decoder (stock: `*param_4 + 8 == 0`).
    let product = UserIrProduct::import_wav(wav, WavPolicy::RecoveredHeader38, limits, metadata)?;
    Ok(StockProduct {
        id: product.id.clone(),
        product_type: ProductType::Ir,
        read_only: false,
        raw_data: Some(product.rawdata()),
        metadata: Some(product.metadata.to_blob(cipher)),
    })
}

/// What [`import_cloud_ir`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudIr {
    /// The new product id.
    Added(String),
    /// A user IR with the same samples (the same id) is in the library, under `name`.
    Present { id: String, name: String },
    /// Another user IR has the name.
    NameTaken,
}

/// A cloud IR download into `folder` (`0x79992c` → `0x799134(.., 0x76be70(root, 1), -1)` →
/// `0x1e8a70` → `0x1e01e8` → `0x1de9f8` → IRs Library `vt+0x54` `0x706f34` → `vt+0x94`
/// `0x82e91c`): the WAV payload through the extractor (`0x72884c` → `0x69c384`, the first
/// 1,024 samples; `0x72884c` sets only the IR data, record `+0x80`), the id `CIR_` + MD5
/// (`0x1de9f8`, `0x829468`), the metadata of `0x7ee62c` case 1 from the cloud record
/// (`0x7240b0`): `name` (`+0x10`), `date` (`+0x28`, `created`; a null date is ""), `cloudId`
/// (`+0x30`, the product id), `corosVersion` (`creator_version`). Linked to the root and
/// `folder`, under the cap (`store_media`), in one transaction.
///
/// O (user data first; stock asks in dialogs not traced: `0x465ba4` when the id is in the
/// library under another name, then the `vt+0x60` name branch of `0x1de9f8`): an IR with the
/// same samples ([`CloudIr::Present`]) or a user IR with the same name ([`CloudIr::NameTaken`],
/// I: exact text; unreadable metadata is skipped) is not written.
pub fn import_cloud_ir(
    db: &mut StockDb,
    folder: i64,
    wav: &[u8],
    cloud: &tetrad_services::cloud::products::CloudProduct,
    cipher: &DeviceCipher,
    limits: IrLimits,
) -> Result<CloudIr> {
    let metadata = UserIrMetadata {
        name: cloud.name.clone(),
        date: cloud.date.map(utc_date).unwrap_or_default(),
        cloud_id: cloud.id.clone(),
        coros_version: cloud.creator_version.clone(),
    };
    let product = ir_product(wav, metadata, cipher, limits)?;
    let view = db.view();
    let name = |row: Option<StockProduct>| {
        let blob = row.and_then(|row| row.metadata).unwrap_or_default();
        UserIrMetadata::from_blob(&blob, cipher).map(|metadata| metadata.name)
    };
    if let Some(held) = view.product(&product.id)? {
        let name = name(Some(held)).unwrap_or_default();
        let id = product.id;
        return Ok(CloudIr::Present { id, name });
    }
    // The rows without rawdata: the rawdata of the other IRs stays unread.
    for (_, row) in view.product_rows(ProductType::Ir)? {
        if name(Some(row?)).is_ok_and(|named| named == cloud.name) {
            return Ok(CloudIr::NameTaken);
        }
    }
    store_media(db, &product, folder)?;
    Ok(CloudIr::Added(product.id))
}

/// The outcome of one legacy migration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyMigration {
    /// Product ids written, in file name order.
    pub products: Vec<String>,
    /// Files left out; they stay in the renamed `_legacy` directory.
    pub skipped: Vec<String>,
    /// Preset files whose IR references were rewritten.
    pub presets: Vec<String>,
}

/// The legacy file names of one directory with `suffix`, sorted. Stock lists
/// with `QDir::entryList` and no sort; the order changes no row.
pub(super) fn legacy_files(host: &std::path::Path, suffix: &str) -> Result<Vec<String>> {
    let mut files: Vec<String> = named_entries(host, false)?
        .into_iter()
        .filter(|(name, is_dir)| !is_dir && name.ends_with(suffix))
        .map(|(name, _)| name)
        .collect();
    files.sort();
    Ok(files)
}

/// `QFileInfo::baseName`: the file name up to its first `.`.
pub(super) fn base_name(file: &str) -> &str {
    let name = file.rsplit('/').next().unwrap_or(file);
    name.split('.').next().unwrap_or(name)
}

impl StockLibrary {
    /// The legacy directory's host path and its `_legacy` target; `None` when
    /// the directory does not exist. An existing `_legacy` target is refused
    /// before any change (stock runs `rm -rf` on it first; Tetrad keeps it).
    fn legacy_dir(&self, device: &str) -> Result<Option<(std::path::PathBuf, std::path::PathBuf)>> {
        let host = self.media.resolve(device)?;
        if !fs::symlink_metadata(&host).is_ok_and(|metadata| metadata.is_dir()) {
            return Ok(None);
        }
        let target = self.media.resolve(&format!("{device}{LEGACY_SUFFIX}"))?;
        if fs::symlink_metadata(&target).is_ok() {
            return Err(Error::Unsupported(
                "a legacy media directory and its _legacy copy both exist",
            ));
        }
        Ok(Some((host, target)))
    }

    /// Boot migration of `/media/p4/CustomIRs/Impulse Responses` (`0x006e72d4`):
    ///
    /// 1. Each `*.bin` becomes a type-1 row: rawdata = the file bytes, id =
    ///    `CIR_` + MD5, name = base name. A `<stem>.json` sidecar is read as IR
    ///    metadata (`0x7efc38`); without one the date is the file creation
    ///    time. Rows are linked to "My IRs", all in one transaction.
    /// 2. Every `*.pb` of every preset folder gets its legacy IR paths
    ///    rewritten ([`rewrite_legacy_ir_references`], `0x006e6138`).
    /// 3. The directory is renamed to `<dir>_legacy` (`0x006e45dc`).
    ///
    /// `model` gives the ModelRepo category and base model id of a model id.
    /// Owned differences: a `.bin` that is not 4-byte aligned or holds more
    /// than 1,024 words is skipped (stock stores it); stock writes a missing
    /// sidecar into the legacy directory, Tetrad does not; stock also sets
    /// the preset version string (`0x2e1d2c`, "4.1.0"), which Tetrad leaves.
    pub fn migrate_legacy_irs(
        &mut self,
        db: &mut StockDb,
        cipher: &DeviceCipher,
        coros_version: &str,
        model: impl Fn(u32) -> Option<(u32, u32)>,
    ) -> Result<Option<LegacyMigration>> {
        let Some((host, target)) = self.legacy_dir(LEGACY_USER_IR_DIR)? else {
            return Ok(None);
        };
        let mut report = LegacyMigration::default();
        let mut rows = Vec::new();
        let mut references = BTreeMap::new();
        let raw_limits = IrLimits {
            max_asset_bytes: MAX_IR_SAMPLES * 4,
            max_chunks: 0,
        };
        for file in legacy_files(&host, ".bin")? {
            let path = host.join(&file);
            let Some(raw) = read_file(&path, self.limits.max_file_bytes)? else {
                continue;
            };
            if decode(&raw, IrInputFormat::RawLeFloat32, raw_limits).is_err() {
                report.skipped.push(file);
                continue;
            }
            let stem = file.strip_suffix(".bin").unwrap_or(&file);
            let sidecar = read_file(
                &host.join(format!("{stem}.json")),
                self.limits.max_file_bytes,
            )?;
            let metadata = match sidecar {
                Some(json) => legacy_ir_metadata(&json, base_name(&file), coros_version),
                None => {
                    let created = fs::metadata(&path)
                        .and_then(|metadata| metadata.created().or_else(|_| metadata.modified()))
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    UserIrMetadata {
                        name: base_name(&file).to_owned(),
                        date: utc_date(created),
                        cloud_id: String::new(),
                        coros_version: coros_version.to_owned(),
                    }
                }
            };
            let id = product_id(&raw);
            references.insert(format!("{LEGACY_USER_IR_DIR}/{file}"), id.clone());
            report.products.push(id.clone());
            rows.push(StockProduct {
                id,
                product_type: ProductType::Ir,
                read_only: false,
                raw_data: Some(raw),
                metadata: Some(metadata.to_blob(cipher)),
            });
        }
        db.transaction(|write| {
            let root = user_root(write, ProductType::Ir)?;
            rows.iter().try_for_each(|row| {
                write.put_product(row)?;
                write.link(root, &row.id)
            })
        })?;
        for setlist in self.setlists()? {
            let host = self.media.resolve(&setlist.path)?;
            for file in legacy_files(&host, ".pb")? {
                let path = format!("{}/{file}", setlist.path);
                let mut preset = self.load(&path)?;
                let changed = preset
                    .document
                    .modify(|preset| rewrite_legacy_ir_references(preset, &references, &model))?;
                if changed {
                    self.save(&mut preset)?;
                    report.presets.push(path);
                }
            }
        }
        fs::rename(host, target)?;
        Ok(Some(report))
    }

    /// Boot migration of `/media/p4/Captures/My Captures` (`0x006e71cc` →
    /// `0x006e6c18`): each `*.cns` with a non-empty `<base name>.json`
    /// becomes a writable type-2 row (id = base name, rawdata and metadata =
    /// the file bytes as stored) linked to "My Captures", in one transaction;
    /// a capture without metadata is skipped ("Skipped file:"). Then the
    /// directory is renamed to `<dir>_legacy`.
    pub fn migrate_legacy_captures(&self, db: &mut StockDb) -> Result<Option<LegacyMigration>> {
        let Some((host, target)) = self.legacy_dir(LEGACY_CAPTURE_DIR)? else {
            return Ok(None);
        };
        let mut report = LegacyMigration::default();
        let mut rows = Vec::new();
        for file in legacy_files(&host, ".cns")? {
            let id = base_name(&file).to_owned();
            let raw = read_file(&host.join(&file), self.limits.max_file_bytes)?;
            let metadata = read_file(&host.join(format!("{id}.json")), self.limits.max_file_bytes)?
                .filter(|json| !json.is_empty());
            let (Some(raw), Some(metadata)) = (raw, metadata) else {
                report.skipped.push(file);
                continue;
            };
            report.products.push(id.clone());
            rows.push(StockProduct {
                id,
                product_type: ProductType::Capture,
                read_only: false,
                raw_data: Some(raw),
                metadata: Some(metadata),
            });
        }
        db.transaction(|write| {
            let root = user_root(write, ProductType::Capture)?;
            rows.iter().try_for_each(|row| {
                write.put_product(row)?;
                write.link(root, &row.id)
            })
        })?;
        fs::rename(host, target)?;
        Ok(Some(report))
    }
}

/// A legacy sidecar read like stock metadata: start from the base name, then
/// take the string fields the JSON has. An unreadable sidecar keeps the
/// defaults. The CorOS version default is inferred.
fn legacy_ir_metadata(json: &[u8], name: &str, coros_version: &str) -> UserIrMetadata {
    let object = serde_json::from_slice::<serde_json::Value>(json).unwrap_or_default();
    let text = |key: &str, default: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(default)
            .to_owned()
    };
    UserIrMetadata {
        name: text("name", name),
        date: text("date", ""),
        cloud_id: text("cloudId", ""),
        coros_version: text("corosVersion", coros_version),
    }
}

/// `0x006e6138` for one preset: in every grid block with IR reference
/// params ([`reference_params`]), each scene value of each id param that
/// contains `CustomIRs` (`0x326980`) becomes the new product id when
/// `references` has it. For IR Loader (category 29) and Parallax (3008) the
/// paired name param also becomes the path's base name (`0x326b4c`), mapped
/// or not. Returns true when a value changed. Pairing id param A with name
/// param A is inferred (Ghidra drops the fifth argument).
pub fn rewrite_legacy_ir_references(
    preset: &mut Preset,
    references: &BTreeMap<String, String>,
    model: impl Fn(u32) -> Option<(u32, u32)>,
) -> tetrad_preset::Result<bool> {
    const IR_LOADER: u32 = 29;
    const PARALLAX: u32 = 3008;
    let mut changed = false;
    for row in 0..ROWS {
        for column in 0..COLUMNS {
            let slot = Slot { row, column };
            let Some((category, base)) = preset
                .block(slot)?
                .and_then(|block| block.hash)
                .and_then(&model)
            else {
                continue;
            };
            let Some(params) = reference_params(category, base) else {
                continue;
            };
            let names = category == IR_LOADER || base == PARALLAX;
            let block = preset.block_mut(slot)?;
            for (id_param, name_param) in params.id.into_iter().zip(params.name) {
                for scene in 0..SCENES {
                    let Some(ParamValue::String(path)) = block
                        .params
                        .get(id_param as usize)
                        .and_then(|param| param.values.get(scene))
                        .and_then(|value| value.value.clone())
                    else {
                        continue;
                    };
                    if !path.contains("CustomIRs") {
                        continue;
                    }
                    let mut set = |index: u32, text: String| {
                        if let Some(value) = block
                            .params
                            .get_mut(index as usize)
                            .and_then(|param| param.values.get_mut(scene))
                            && value.value.as_ref() != Some(&ParamValue::String(text.clone()))
                        {
                            value.value = Some(ParamValue::String(text));
                            changed = true;
                        }
                    };
                    if let Some(id) = references.get(&path) {
                        set(id_param, id.clone());
                    }
                    if names {
                        set(name_param, base_name(&path).to_owned());
                    }
                }
            }
        }
    }
    Ok(changed)
}

/// Translation of both capture stores to `target`, downloads then user (`0x006a6358`,
/// `0x006a68f4`), as the restore (`0x003fcf64`, to local) and the backup (`0x004770f4`,
/// to global) run it: every store not flagged with the target is re-keyed. `from` and `to`
/// are the matching ciphers: to local, `from` = seed only (mode 0) and `to` = seed and serial
/// (mode 1); to global, the reverse. Boot uses [`translate_global_stores`].
pub fn translate_capture_stores(
    db: &mut StockDb,
    target: CaptureEncryption,
    from: &DeviceCipher,
    to: &DeviceCipher,
) -> Result<[StoreTranslation; 2]> {
    Ok([
        translate_store(db, CaptureStore::Downloads, target, from, to)?,
        translate_store(db, CaptureStore::User, target, from, to)?,
    ])
}

/// The boot translation (`0x006a6f78`): user, then downloads, each re-keyed from `global`
/// to `local` only when its flag is global (2). Any other flag is left as it is: an unset
/// store is device-keyed already (C: a CorOS 4.1.0 unit keeps `nc_encryption` empty and its
/// rows read with the device cipher; B13 boot 761). Returns `[downloads, user]`, `None` for a
/// store that is not global.
pub fn translate_global_stores(
    db: &mut StockDb,
    global: &DeviceCipher,
    local: &DeviceCipher,
) -> Result<[Option<StoreTranslation>; 2]> {
    let mut run = |store| -> Result<Option<StoreTranslation>> {
        if db.view().capture_encryption(store)? != CaptureEncryption::Global {
            return Ok(None);
        }
        translate_store(db, store, CaptureEncryption::Local, global, local).map(Some)
    };
    let user = run(CaptureStore::User)?;
    Ok([run(CaptureStore::Downloads)?, user])
}

fn translate_store(
    db: &mut StockDb,
    store: CaptureStore,
    target: CaptureEncryption,
    from: &DeviceCipher,
    to: &DeviceCipher,
) -> Result<StoreTranslation> {
    let outcome = db.translate_capture_store(store, target, |row| {
        match (&mut row.raw_data, &mut row.metadata) {
            (Some(raw), Some(metadata)) => {
                translate_capture(from, to, raw, metadata);
            }
            (None, Some(metadata)) => {
                translate_capture(from, to, &mut [], metadata);
            }
            (_, None) => {}
        }
        Ok(())
    })?;
    Ok(outcome)
}
