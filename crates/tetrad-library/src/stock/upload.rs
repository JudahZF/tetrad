//! The parts of a user capture upload (re-r34 §1, `0x7bd384`): the payload record in the cloud
//! cipher and the plain record metadata. Keys are handles only ([`DeviceCipher`]).

use tetrad_capture::stored::v1_runtime_data;
use tetrad_capture::v2::Capture2File;
use tetrad_media::factory::DeviceCipher;
use tetrad_storage::stock::{CaptureStore, ProductType, StockDb};

use super::{CaptureSource, find_capture};
use crate::sidecar::Sidecar;
use crate::{Error, Result};

/// The capture keys of an upload, as handles (never key bytes).
pub struct UploadKeys<'a> {
    /// ZC mode 1 (seed and serial): the stored row (`0x9f4fe0(1, ..)` decrypts it).
    pub local: &'a DeviceCipher,
    /// ZC mode 0 (seed only): the cloud record (`0x9f4e28(0, ..)` encrypts it).
    pub global: &'a DeviceCipher,
    /// The DSP boot vector `0x27` key: an NC2 record must load (`+0x78`) to give its size.
    pub nc2: Option<&'a DeviceCipher>,
}

/// One user capture ready for `ProductPayload::Capture { id: hash, record, metadata }`.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureUploadParts {
    /// The record's case-2 fields (`0x7ee62c(record, 0)`), `parametersBlockLength` set to the
    /// runtime data size.
    pub fields: Sidecar,
    /// `<hash>.cns`: V1 re-keyed from the device cipher to the seed-only cipher; NC2 as stored.
    pub record: Vec<u8>,
    /// `<hash>.json`: `fields` as plain JSON (mode 0 writes no cipher pass).
    pub metadata: Vec<u8>,
}

/// The upload parts of the capture `hash` (`0x7bd384`), found as [`find_capture`] finds it
/// (`0x6f6484`): a user row of type 2, not read-only, or a Downloads row (re-r38 §1: the
/// preset walk uploads the account's own Downloads capture the same way), in a local store
/// (O; the Downloads store is local-keyed after the boot translation). The record version
/// decides the payload: not `"2"` → `0x6ae59c` (the local → global re-key, C); `"2"` → no
/// cipher pass (C), but the record must load with `keys.nc2` (`0x706f34` → `0x7ec448`). O: a
/// version other than `"1"` and `"2"` is refused (stock would run the re-key on it).
pub fn capture_upload(
    db: &StockDb,
    hash: &str,
    keys: &UploadKeys<'_>,
) -> Result<CaptureUploadParts> {
    let view = db.view();
    let (store, raw, metadata) = match find_capture(&view, None, hash)? {
        Some(CaptureSource::User(row)) if row.read_only => {
            return Err(tetrad_storage::Error::ReadOnly.into());
        }
        Some(CaptureSource::User(row)) => (CaptureStore::User, row.raw_data, row.metadata),
        Some(CaptureSource::Download(row)) => (CaptureStore::Downloads, row.raw_data, row.metadata),
        Some(CaptureSource::Factory(_)) | None => {
            return Err(tetrad_storage::Error::ProductMissing(hash.to_owned()).into());
        }
    };
    if !view.capture_encryption(store)?.is_device_keyed() {
        return Err(Error::Unsupported(
            "a capture upload while its capture store is not device-keyed",
        ));
    }
    let mut plain = metadata.unwrap_or_default();
    keys.local.apply(&mut plain);
    let mut fields = Sidecar::parse(&plain)?;
    let mut record = raw.ok_or(Error::Unsupported("a capture row without rawdata"))?;
    let unloaded = Error::Unsupported("the stored capture record does not load");
    let runtime = match fields.text("version").unwrap_or("1") {
        "1" => {
            keys.local.apply(&mut record);
            let runtime = v1_runtime_data(&record).map_err(|_| unloaded)?;
            keys.global.apply(&mut record);
            runtime.len()
        }
        "2" => {
            let nc2 = keys.nc2.ok_or(Error::Unsupported(
                "an NC2 capture upload before DSP vector 0x27",
            ))?;
            let mut loaded = record.clone();
            nc2.apply(&mut loaded);
            let file = Capture2File::parse(&loaded).map_err(|_| unloaded)?;
            file.sharc_blob().len()
        }
        _ => {
            return Err(Error::Unsupported(
                "a capture upload of an unknown record version",
            ));
        }
    };
    fields.set_text("parametersBlockLength", &runtime.to_string());
    let metadata = fields.encode();
    Ok(CaptureUploadParts {
        fields,
        record,
        metadata,
    })
}

/// The cloud id after a good upload reply (`0x1d5944` → `0x7dd13c(record, id)` → `0x82ddfc`:
/// `UPDATE product SET metadata`): the record's `cloudUUID`, in the device cipher, one
/// transaction; the rawdata stays.
pub fn set_capture_cloud_id(
    db: &mut StockDb,
    hash: &str,
    cloud_id: &str,
    cipher: &DeviceCipher,
) -> Result<()> {
    let written = db.transaction(|write| {
        let row = write
            .product(hash)?
            .filter(|row| row.product_type == ProductType::Capture && !row.read_only)
            .ok_or_else(|| tetrad_storage::Error::ProductMissing(hash.to_owned()))?;
        let mut plain = row.metadata.unwrap_or_default();
        cipher.apply(&mut plain);
        let Ok(mut fields) = Sidecar::parse(&plain) else {
            return Ok(false);
        };
        fields.set_text("cloudUUID", cloud_id);
        let mut sealed = fields.encode();
        cipher.apply(&mut sealed);
        write.set_product_metadata(hash, Some(&sealed))?;
        Ok(true)
    })?;
    match written {
        true => Ok(()),
        false => Err(Error::Unsupported("the capture metadata does not read")),
    }
}
