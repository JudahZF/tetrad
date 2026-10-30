//! The two Downloads folders: their stock keys (Control File listings), the row of a cloud product
//! and its store entry.

use serde_json::Value;
use tetrad_capture::stored::{
    device_type_code, device_type_name, instrument_type_code, instrument_type_name, v1_runtime_data,
};
use tetrad_capture::v1::{Limits, RuntimeData};
use tetrad_capture::v2::Capture2File;
use tetrad_media::factory::DeviceCipher;
use tetrad_services::cloud::products::{CloudProduct, ProductPayload};
use tetrad_services::cloud::services::ProductKind;
use tetrad_storage::stock::{
    CaptureEncryption, CaptureStore, Download, DownloadKind, ProductType, StockDb, StockProduct,
};

use super::conversion::{MAX_RECORD, capture_record};
use super::media::{store_in, user_root};
use super::{Committed, StockLibrary, remove_preset_files, stored_name};
use crate::sidecar::{Sidecar, utc_date};
use crate::{Error, Result};

/// One Downloads folder. `0x0076ff74` builds both (`0x00215f98` presets, `0x00214ee8`
/// captures); the base `0x00716bac` sets the key to `"cloud-%1-%2"` with the folder type
/// and 1 (Downloads; 0 is the cloud browse folder), read back by vtable `+0x34`
/// (`0x00716208`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadsFolder {
    pub kind: DownloadKind,
    /// The folder type and Control `file_type` (vtable `+0x30`, `0x007161f0`).
    pub file_type: i32,
    pub key: &'static str,
}

/// Presets first, then captures: the order of the resync listing (`0x0025057c`).
/// Control lists a folder as `0x0024592c` does: `FolderInfo` with the key,
/// `is_factory` false, `is_downloads` true and no name, then one `ProductData` per row.
pub const DOWNLOADS_FOLDERS: [DownloadsFolder; 2] = [
    DownloadsFolder {
        kind: DownloadKind::Preset,
        file_type: 0,
        key: "cloud-0-1",
    },
    DownloadsFolder {
        kind: DownloadKind::Capture,
        file_type: 2,
        key: "cloud-2-1",
    },
];

/// The Downloads folder of a Control folder key.
pub fn downloads_folder(key: &str) -> Option<&'static DownloadsFolder> {
    DOWNLOADS_FOLDERS.iter().find(|folder| folder.key == key)
}

/// More items than this in a Downloads folder show the "Excessive %1 in Downloads folder"
/// dialog (`0x22cab0`: folder count, vtable `+0x80`, `> 0x200`).
pub const EXCESSIVE_DOWNLOADS: usize = 512;

/// The Downloads folder whose "Excessive %1 in Downloads folder" dialog (`0x22bd50`) shows, as
/// `0x22cab0` picks it: captures first (dialog type 2), else presets (type 0), each only when
/// above [`EXCESSIVE_DOWNLOADS`] and not `suppressed`. The caller gives the "Don't show
/// again" flags (`dontShowExcessCaptureDownloads` `+0x1ba`, `dontShowExcessPresetDownloads`
/// `+0x1b9`). Stock runs the check at boot after the What's New popup (`0x54491c`,
/// `0x3515f8`) and at the end of the restore reload (`0x3fcf64`). I: the folder count is the
/// row count of its Downloads table.
pub fn excessive_downloads(
    db: &StockDb,
    suppressed: impl Fn(DownloadKind) -> bool,
) -> Result<Option<DownloadKind>> {
    for kind in [DownloadKind::Capture, DownloadKind::Preset] {
        if !suppressed(kind) && db.view().download_ids(kind)?.len() > EXCESSIVE_DOWNLOADS {
            return Ok(Some(kind));
        }
    }
    Ok(None)
}

/// What [`store_download`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredDownload {
    Added,
    /// A row with the same id was there (stock `REPLACE INTO`).
    Replaced,
}

/// Stores a checked cloud product into its Downloads table (`0x007eb270` →
/// `0x0069fc7c`). The caller ran the corrupt check first (`0x0071d538`) and gives
/// capture bytes in the downloads store's cipher. A capture needs its `hash`, a preset has
/// none.
///
/// Owned: a capture is refused unless the downloads store is flagged local, so a pending
/// store translation never re-encrypts a row that is already local.
pub fn store_download(
    db: &mut StockDb,
    kind: DownloadKind,
    item: &Download,
) -> Result<StoredDownload> {
    if kind == DownloadKind::Capture
        && !(db.view().capture_encryption(CaptureStore::Downloads)?).is_device_keyed()
    {
        return Err(Error::Unsupported(
            "a capture download while the downloads store is not device-keyed",
        ));
    }
    Ok(db.transaction(|write| {
        let replaced = write.download_ids(kind)?.contains(&item.id);
        write.put_download(kind, item)?;
        Ok(if replaced {
            StoredDownload::Replaced
        } else {
            StoredDownload::Added
        })
    })?)
}

/// What [`store_library_capture`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryCapture {
    Added,
    /// The Captures Library holds the hash already (`0x1d8bac`: `vt+0x44` → no add).
    Present,
}

/// Adds the capture row of a download ([`download_row`]) to the Captures Library, as a
/// preset's capture dependency does (`0x1d8bac` → `vt+0x54` `0x706f34` → `vt+0x94`
/// `0x82b63c` → `0x82e91c`): the same rawdata and metadata as the Downloads row (`0x7eced4`,
/// `0x7ee62c(record, 1)`), keyed by its NC hash (`vt+0x98` `0x8297b0`: record `+4`; I: the
/// row `hash`), linked to the capture user root (root `+0x18`, not the saving destination),
/// under the 2,048-item cap (`0x82e91c`: `vt+0x74` full → 2), in one transaction. A hash the
/// library holds is not added ([`LibraryCapture::Present`]). O: refused unless the user
/// capture store is local (the row is in the device cipher), as [`store_download`].
pub fn store_library_capture(db: &mut StockDb, row: &Download) -> Result<LibraryCapture> {
    let hash = (row.hash.clone())
        .filter(|hash| !hash.is_empty())
        .ok_or(Error::Unsupported("a capture download without its hash"))?;
    if !db
        .view()
        .capture_encryption(CaptureStore::User)?
        .is_device_keyed()
    {
        return Err(Error::Unsupported(
            "a capture dependency while the user capture store is not device-keyed",
        ));
    }
    let product = StockProduct {
        id: hash,
        product_type: ProductType::Capture,
        read_only: false,
        raw_data: row.raw_data.clone(),
        metadata: row.metadata.clone(),
    };
    Ok(db.transaction(|write| {
        if write.product(&product.id)?.is_some() {
            return Ok(LibraryCapture::Present);
        }
        let root = user_root(write, ProductType::Capture)?;
        store_in(write, &product, root)?;
        Ok(LibraryCapture::Added)
    })?)
}

/// Where a cloud capture download goes in the user capture store, as stock sorts it: the
/// destination check of `0x1e5f24`, then `0x6f8584` and `0x1e2bf4`, with the stock status
/// codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudCapture {
    /// Status 3: A new hash: added to the destination folder (and the user root).
    Added,
    /// Status 7: The store holds the hash with the same name and tags: the destination folder links
    /// it now (`0x1d8bac` adds only to a folder without the key; no row is written).
    Linked,
    /// The destination folder holds the hash (`0x1e5f24` `vt+0x44`): nothing.
    Present,
    /// Status 4: A new hash, and a user capture has its name (`0x464e30` mode 1): nothing.
    NameTaken,
    /// Status 6: The store holds the hash, and another capture has the cloud name: nothing.
    NameOfOther,
    /// Status 9: A factory capture: nothing.
    Factory,
    /// Statuses 5 and 8: The store holds the hash under another name (or other tags): stock asks
    /// (`0x465178` mode 1) before [`overwrite_capture_metadata`]. `existing` is the stored
    /// name (`%2`).
    Ask { existing: String },
}

/// The name and tags of a user capture's metadata (device cipher, `0x7ee62c` case 2).
fn capture_name_tags(metadata: &[u8], cipher: &DeviceCipher) -> Result<(String, Vec<String>)> {
    let mut plain = metadata.to_vec();
    cipher.apply(&mut plain);
    let sidecar = Sidecar::parse(&plain)?;
    let tags = sidecar
        .texts("tags")
        .into_iter()
        .map(str::to_owned)
        .collect();
    Ok((sidecar.text("name").unwrap_or_default().to_owned(), tags))
}

/// A Directory CLOUD or Control 46 capture download into the NC saving destination `folder`
/// (`0x79992c` / `0xf3f24` → `0x1d9954(.., 0x76be70(root, 2), ..)` → `0x1e5f24` →
/// `0x6f8584` → `0x1e2bf4`), sorted and stored in one transaction. `row` is its Downloads
/// row ([`download_row`]: rawdata and metadata in the device cipher, `cipher`); `name` and
/// `tags` are the cloud record's. `factory`: the caller found the hash in `factory.db`
/// (`0x765550`/`0x765568`). Status 3 stores the row as [`store_library_capture`] does, but into
/// `folder`; every other status writes no row (C). O: refused unless the user capture store
/// is local; a user capture whose metadata does not read is skipped in the name search, a
/// stored row with the hash that does not read is an error.
pub fn store_cloud_capture(
    db: &mut StockDb,
    row: &Download,
    name: &str,
    tags: &[String],
    folder: i64,
    factory: bool,
    cipher: &DeviceCipher,
) -> Result<CloudCapture> {
    let hash = (row.hash.clone())
        .filter(|hash| !hash.is_empty())
        .ok_or(Error::Unsupported("a capture download without its hash"))?;
    if !db
        .view()
        .capture_encryption(CaptureStore::User)?
        .is_device_keyed()
    {
        return Err(Error::Unsupported(
            "a cloud capture while the user capture store is not device-keyed",
        ));
    }
    // The outer `Result` is the transaction; the inner one a refusal that writes nothing.
    db.transaction(|write| {
        if write.folder_products(folder)?.contains(&hash) {
            return Ok(Ok(CloudCapture::Present));
        }
        if factory {
            return Ok(Ok(CloudCapture::Factory));
        }
        // The rows without rawdata: the rawdata of the other captures stays unread.
        let mut other_named = false;
        for (other, row) in write.product_rows(ProductType::Capture)? {
            if other == hash {
                continue;
            }
            if capture_name_tags(&row?.metadata.unwrap_or_default(), cipher)
                .is_ok_and(|(other_name, _)| other_name == name)
            {
                other_named = true;
                break;
            }
        }
        let Some(stored) = write.product(&hash)? else {
            if other_named {
                return Ok(Ok(CloudCapture::NameTaken));
            }
            let product = StockProduct {
                id: hash.clone(),
                product_type: ProductType::Capture,
                read_only: false,
                raw_data: row.raw_data.clone(),
                metadata: row.metadata.clone(),
            };
            store_in(write, &product, folder)?;
            return Ok(Ok(CloudCapture::Added));
        };
        if other_named {
            return Ok(Ok(CloudCapture::NameOfOther));
        }
        let Ok((stored_name, stored_tags)) =
            capture_name_tags(&stored.metadata.unwrap_or_default(), cipher)
        else {
            return Ok(Err(Error::Unsupported(
                "the stored capture with this hash has metadata that does not read",
            )));
        };
        if stored_name == name && stored_tags == tags {
            write.link(folder, &hash)?;
            return Ok(Ok(CloudCapture::Linked));
        }
        Ok(Ok(CloudCapture::Ask {
            existing: stored_name,
        }))
    })?
}

/// What a Downloads capture MOVE or COPY did ([`move_download_capture`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadMove {
    /// A new hash: the row is now a user capture in the target folder (and the user root).
    Stored,
    /// The user store held the hash: the target folder links it now.
    Linked,
    /// The target folder held the hash already.
    Present,
}

/// A MOVE or COPY of the Downloads capture `key` (its capture hash, as the listing keys it,
/// or its cloud id) into the user capture folder `folder`, then the Downloads row goes, in one
/// transaction. Stock (C): `0x24b254` → `0x1e8a70` (a cloud source, target type 2) →
/// `0x1d9954` → `0x1d94d4`: when the device holds the capture (`0x7db500`, the transfer
/// manager's index of held items, filled by `0x7dbac4`), the holder (`0x7dbfb8`) gives its
/// record (`vt+0x40`) and the target adds it unless it holds it (`vt+0x44`, `vt+0x54` =
/// `0x706f34`: a record with data → `vt+0x94` `0x82e91c`, a plain store with no name check and
/// no question; else a link of the key, `vt+0x58`); then a Downloads source removes the key
/// (`vt+0x4c`). MOVE and COPY act the same. Tetrad: the holder is the user store when it has
/// the hash (I: stock indexes by key), else the Downloads row (its rawdata and metadata, in
/// the device cipher as the user store); the capacity gate `0x1d6a80` is
/// [`super::MEDIA_LIBRARY_CAP`]. Refused: a key with no Downloads row, a row without a hash,
/// a user capture store that is not device-keyed, a target that is not a writable capture
/// folder.
pub fn move_download_capture(db: &mut StockDb, key: &str, folder: i64) -> Result<DownloadMove> {
    if !db
        .view()
        .capture_encryption(CaptureStore::User)?
        .is_device_keyed()
    {
        return Err(Error::Unsupported(
            "a Downloads capture while the user capture store is not device-keyed",
        ));
    }
    Ok(db.transaction(|write| {
        let row = (write.download(DownloadKind::Capture, key)?)
            .ok_or_else(|| tetrad_storage::Error::ProductMissing(key.to_owned()))?;
        let hash = (row.hash.clone())
            .filter(|hash| !hash.is_empty())
            .ok_or(tetrad_storage::Error::Identity)?;
        let moved = if write.folder_products(folder)?.contains(&hash) {
            DownloadMove::Present
        } else if write.product(&hash)?.is_some() {
            write.link(folder, &hash)?;
            DownloadMove::Linked
        } else {
            let product = StockProduct {
                id: hash,
                product_type: ProductType::Capture,
                read_only: false,
                raw_data: row.raw_data,
                metadata: row.metadata,
            };
            store_in(write, &product, folder)?;
            DownloadMove::Stored
        };
        write.delete_download(DownloadKind::Capture, &row.id)?;
        Ok(moved)
    })?)
}

/// OVERWRITE of "Neural Capture %3 already exists on your %1 (%2)" (`0x1d8f48` →
/// `0x1d8bac(app, 1, ..)` → `vt+0x50` `0x82ddfc`): only `UPDATE product SET metadata` of the
/// user capture `hash` with the cloud record's metadata (`0x7ee62c(record, 1)`, device
/// cipher). Its rawdata and every other capture stay (C). One transaction. Refused: a hash
/// the store does not hold (`0x82ddfc`: not in the folder map → 2), a read-only row, a row
/// that is not a capture, and (O) a store that is not local.
pub fn overwrite_capture_metadata(db: &mut StockDb, hash: &str, metadata: &[u8]) -> Result<()> {
    if db.view().capture_encryption(CaptureStore::User)? != CaptureEncryption::Local {
        return Err(Error::Unsupported(
            "a capture overwrite while the user capture store is not local",
        ));
    }
    db.transaction(|write| {
        match write.product(hash)? {
            Some(row) if row.product_type == ProductType::Capture && !row.read_only => {}
            Some(_) => return Err(tetrad_storage::Error::ReadOnly),
            None => return Err(tetrad_storage::Error::ProductMissing(hash.to_owned())),
        }
        write.set_product_metadata(hash, Some(metadata))
    })
    .map_err(Error::from)
}

/// The capture keys of a download, as handles (never key bytes).
pub struct DownloadKeys<'a> {
    /// ZC secret slot 0 (the seed alone): a V1 capture download is in it (`0x7bb860` →
    /// `0x6accdc` → `0x9f4fe0(0, …)`).
    pub global: &'a DeviceCipher,
    /// ZC slot 1 (seed and serial): the Downloads row (`0x6a974c`).
    pub local: &'a DeviceCipher,
    /// The DSP boot vector `0x27` key: NC2 rawdata both ways (`0x6acc9c`, `0x6ae55c`).
    pub nc2: Option<&'a DeviceCipher>,
}

/// The Downloads row of a downloaded product: the folder check (vtable `+0x98`; `0x71d538`
/// names a failure "Downloaded product %1 (%2) is corrupt."), then `0x7eb270`: rawdata
/// `0x7eced4`, metadata `0x7ee62c(record, 1)`. A preset row is `id` = product id, no hash;
/// a capture row is `id` = cloud id, `hash` = the base name of the payload's first member.
/// An IR has no Downloads table (`Unsupported`); a capture needs `keys`.
///
/// I: a preset is stored as downloaded once it parses (stock parses and serializes again,
/// `0x681ad4`; the cloud bytes are a stock serialization).
pub fn download_row(
    product: &CloudProduct,
    payload: &ProductPayload,
    keys: Option<&DownloadKeys<'_>>,
) -> Result<(DownloadKind, Download)> {
    let corrupt = |reason: String| Error::CorruptDownload {
        name: product.name.clone(),
        id: product.id.clone(),
        reason,
    };
    let date = product.date.map(utc_date).unwrap_or_default();
    let tags = Value::from(product.tags.clone());
    let instrument =
        instrument_type_name(instrument_type_code(&product.instrument_type).unwrap_or(0));
    match (product.kind, payload) {
        (ProductKind::Preset, ProductPayload::Preset(bytes)) => {
            tetrad_preset::PresetDocument::parse(bytes, tetrad_preset::Limits::default())
                .map_err(|error| corrupt(error.to_string()))?;
            let mut sidecar = Sidecar::default();
            for (key, value) in [
                ("name", &product.name),
                ("author", &product.author_username),
                ("authorId", &product.author_id),
                ("date", &date),
                ("instrument_type", &instrument.to_owned()),
                ("cloudId", &product.id),
                ("corosVersion", &product.creator_version),
                ("description", &product.description),
                ("creator", &product.creator),
            ] {
                sidecar.set_text(key, value);
            }
            sidecar.fields.extend(product.metadata.clone());
            sidecar.fields.insert("tags".into(), tags);
            let row = Download {
                id: product.id.clone(),
                hash: None,
                raw_data: Some(bytes.clone()),
                metadata: Some(sidecar.encode()),
            };
            Ok((DownloadKind::Preset, row))
        }
        (ProductKind::NeuralCapture, ProductPayload::Capture { id, record, .. }) => {
            let keys = keys.ok_or(Error::Unsupported("a capture download without its ciphers"))?;
            let (runtime, mut raw) = match product.version.as_str() {
                "1" => v1_record(record, keys).map_err(corrupt)?,
                "2" => {
                    let nc2 = (keys.nc2)
                        .ok_or(Error::Unsupported("an NC2 capture before DSP vector 0x27"))?;
                    nc2_record(record, nc2).map_err(corrupt)?
                }
                // O (review r26 S9): stock decrypts any other version with the DSP `0x27`
                // key and parses it as V1 (`0x7bb860` → `0x6ec290`); Tetrad calls it corrupt.
                other => return Err(corrupt(format!("capture version {other:?}"))),
            };
            if product.version == "1" {
                keys.local.apply(&mut raw);
            }
            let mut sidecar = Sidecar::default();
            let path = format!("{id}.cns");
            let block_length = runtime.len().to_string();
            let texts: [(&str, &str); 15] = [
                ("version", &product.version),
                ("name", &product.name),
                ("authorName", &product.author_username),
                ("corosVersion", &product.creator_version),
                ("authorId", &product.author_id),
                ("hash", id),
                ("cloudUUID", &product.id),
                ("description", &product.description),
                ("creator", &product.creator),
                (
                    "deviceType",
                    device_type_name(device_type_code(&product.device_type).unwrap_or(0)),
                ),
                ("instrumentType", instrument),
                ("parametersBlockLength", &block_length),
                ("encryption", "Local"),
                ("date", &date),
                ("path", &path),
            ];
            for (key, value) in texts {
                sidecar.set_text(key, value);
            }
            if product.gain_type >= 0 {
                sidecar
                    .fields
                    .insert("gainType".into(), product.gain_type.into());
            }
            sidecar.fields.insert("tags".into(), tags);
            let mut metadata = sidecar.encode();
            keys.local.apply(&mut metadata);
            let row = Download {
                id: product.id.clone(),
                hash: Some(id.clone()),
                raw_data: Some(raw),
                metadata: Some(metadata),
            };
            Ok((DownloadKind::Capture, row))
        }
        (ProductKind::ImpulseResponse, _) => Err(Error::Unsupported(
            "an IR download: no Downloads table holds IRs",
        )),
        (kind, payload) => Err(corrupt(format!("{payload:?} for a {}", kind.as_str()))),
    }
}

impl StockLibrary {
    /// Stores a downloaded cloud preset into slot `slot` of `setlist` (`0x1dd61c` → cloud
    /// folder `vt+0x40` → `0x1dcf2c` → `0x742990`): the preset and the sidecar of its
    /// Downloads row ([`download_row`]: name, author, `cloudId`, … and the product metadata)
    /// become a free `<name>.pb` with the hash of its path (`0x21ee8c`), then the slot names
    /// it. Write-first: a failed setlist write drops the new files again.
    ///
    /// `replace` is what the slot held when the download was asked: `None` for an empty
    /// slot, else the path the user agreed to overwrite (OVERWRITE of `0x46485c(name, 1)`,
    /// "Preset will be saved overwriting existing preset %1."; `0x1d8a04` runs the same store
    /// `0x742990`). An occupied slot is then replaced as [`Self::replace`] does (new files
    /// first; the old preset, its favorites and recents go when no other slot names it); a
    /// failed clean-up after that is the `tail_error`. Refused before any write (O): an
    /// occupied slot without `replace`, and a slot that no longer holds `replace`
    /// ([`Error::SlotChanged`]).
    pub fn store_cloud_preset(
        &mut self,
        setlist: &str,
        slot: usize,
        product: &CloudProduct,
        payload: &ProductPayload,
        replace: Option<&str>,
    ) -> Result<Committed<String>> {
        let (DownloadKind::Preset, row) = download_row(product, payload, None)? else {
            return Err(Error::Unsupported("a cloud product that is not a preset"));
        };
        self.store_download_preset(setlist, slot, &row, replace)
    }

    /// Stores the preset of a Downloads-format row (`rawdata` the preset, `metadata` its
    /// sidecar) into slot `slot` of `setlist`, with the rules of [`Self::store_cloud_preset`].
    /// A Downloads preset moved into a setlist (`0x1dd61c` from a Downloads source) stores its
    /// own row; the caller then deletes the row (`0x1dc14c`).
    pub fn store_download_preset(
        &mut self,
        setlist: &str,
        slot: usize,
        row: &Download,
        replace: Option<&str>,
    ) -> Result<Committed<String>> {
        let mut slots = self.slots(setlist)?;
        match (slots.get(slot), replace) {
            (None, _) => return Err(Error::SlotRange(slot)),
            (Some(None), None) => {}
            (Some(Some(_)), None) => return Err(Error::SlotOccupied(slot)),
            (Some(held), Some(replace)) if held.as_deref() != Some(replace) => {
                return Err(Error::SlotChanged(slot));
            }
            (Some(_), Some(_)) => {}
        }
        let document = tetrad_preset::PresetDocument::parse(
            row.raw_data.as_deref().unwrap_or_default(),
            self.limits.preset,
        )?;
        let sidecar = Sidecar::parse(row.metadata.as_deref().unwrap_or_default())?;
        let name = stored_name(&document, Some(&sidecar))
            .ok_or(Error::Unsupported("a cloud preset without a name"))?;
        if replace.is_some() {
            return self.replace_committed(setlist, slot, &name, &document, &sidecar, true);
        }
        let (path, file) = self
            .stage_named(&self.setlist_path(setlist)?, &name, &document, &sidecar)?
            .commit()?;
        slots[slot] = Some(path.clone());
        if let Err(error) = self.write_slots(setlist, &slots) {
            let _ = remove_preset_files(&file);
            return Err(error);
        }
        Ok(Committed {
            search_index: self.update_search_index(&[&path], &[]),
            value: path,
            tail_error: None,
        })
    }
}

/// A V1 record: the download in the seed-only cipher (`0x6accdc`) through `0x6ec290`, then
/// serialized again (`0x6ef594`, size `1..=0x2fff`). Returns the runtime data and the plain
/// record.
fn v1_record(
    record: &[u8],
    keys: &DownloadKeys<'_>,
) -> std::result::Result<(Vec<u8>, Vec<u8>), String> {
    let mut plain = record.to_vec();
    keys.global.apply(&mut plain);
    let runtime = v1_runtime_data(&plain).map_err(|error| error.to_string())?;
    let data = RuntimeData::parse(&runtime, Limits::default()).map_err(|e| e.to_string())?;
    let stored = capture_record(&data);
    if !(1..=MAX_RECORD).contains(&stored.len()) {
        return Err(format!("record size {} is out of range", stored.len()));
    }
    Ok((runtime, stored))
}

/// An NC2 record: decrypt (`0x6acc9c`), `0x6f2b60` gives identity ‖ data (the runtime data),
/// then `0x6f4480` writes a new `snng_obfuscate.ObfuscatedGraph` (`Capture2.proto` is proto3:
/// zero scalars and empty fields left out) with version 0.1.0 and no metadata strings, and
/// `0x6ae55c` encrypts it. Returns the runtime data and the encrypted record.
fn nc2_record(
    record: &[u8],
    nc2: &DeviceCipher,
) -> std::result::Result<(Vec<u8>, Vec<u8>), String> {
    let mut plain = record.to_vec();
    nc2.apply(&mut plain);
    let file = Capture2File::parse(&plain).map_err(|error| error.to_string())?;
    let mut stored = vec![0x0a, 0x02, 0x10, 0x01];
    if file.identity != 0 {
        stored.push(0x1d);
        stored.extend(file.identity.to_le_bytes());
    }
    if !file.data.is_empty() {
        stored.push(0x22);
        let mut len = file.data.len() as u64;
        while len >= 0x80 {
            stored.push(len as u8 | 0x80);
            len >>= 7;
        }
        stored.push(len as u8);
        stored.extend(file.data);
    }
    nc2.apply(&mut stored);
    Ok((file.sharc_blob(), stored))
}
