//! User IRs: `zenui.db` products of type 1, their names, preset references and
//! the favorites/recents lists. `tetrad-storage` owns the database rows; this
//! module owns the row contents.
//!
//! - Import (Cortex Control handler VA 0x24dae0, cloud download VA 0x72884c):
//!   WAV bytes → extractor VA 0x69c384 with a 1,024-sample limit → float words.
//! - `rawdata` is the selected LE float32 words, no header, not encrypted.
//! - The id is `CIR_` + MD5(rawdata), each byte as lowercase hex without zero
//!   padding (`QString::number(b, 16)`, VA 0x829468).
//! - `metadata` is JSON `{name, date, cloudId, corosVersion}` (VA 0x7ee62c case
//!   1), encrypted with the device cipher (VA 0x6a974c).

use std::fmt::Write as _;

use md5::{Digest, Md5};

use crate::block::{IR_NAME_PARAMS, IR_PATH_PARAMS};
use crate::factory::DeviceCipher;
use crate::ir::{IrError, IrInputFormat, IrLimits, IrSamples, MAX_IR_SAMPLES, WavPolicy, decode};

pub const USER_IR_PRODUCT_TYPE: i64 = 1;
pub const CUSTOM_IR_PREFIX: &str = "CIR_";
/// Pre-4.x per-file store (`<name>.bin` raw float32 + `<name>.json`), migrated
/// into the database by VA 0x6e72d4.
pub const LEGACY_USER_IR_DIR: &str = "/media/p4/CustomIRs/Impulse Responses";
pub const USER_IR_FOLDERS_PATH: &str = "/media/p4/user_ir_folders";
pub const FAVORITES_IR_PATH: &str = "/media/p4/favorites-ir";
pub const RECENTS_IR_PATH: &str = "/media/p4/recents-ir";
/// Rename dialog limits (VA 0x7e4dc0 max length 60; VA 0x7e5874 Save needs 4).
/// Lengths count UTF-16 units, as `QString::length` does.
pub const MIN_IR_NAME_UNITS: usize = 4;
pub const MAX_IR_NAME_UNITS: usize = 60;
/// Selector label when a referenced IR and its stored name are both gone.
pub const DELETED_IR_LABEL: &str = "DELETED IR";

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum UserIrError {
    #[error(transparent)]
    Ir(#[from] IrError),
    #[error("user IR rawdata has {0} words; at most 1024 are stored")]
    RawdataLength(usize),
    #[error("IR name is empty")]
    EmptyName,
    #[error("IR name has {0} UTF-16 units; the dialog needs 4 to 60")]
    NameLength(usize),
    #[error("IR metadata is neither plain nor device-encrypted JSON")]
    MetadataJson,
    #[error("IR metadata field {0} is missing or not a string")]
    MetadataField(&'static str),
    #[error("IR list has an odd number of lines")]
    CorruptList,
}

/// Stock id for `rawdata` (see the module docs).
pub fn product_id(rawdata: &[u8]) -> String {
    Md5::digest(rawdata)
        .iter()
        .fold(String::from(CUSTOM_IR_PREFIX), |mut id, byte| {
            write!(id, "{byte:x}").expect("String write");
            id
        })
}

/// Fields of the type-1 metadata object. `date` is `yyyy-MM-ddThh:mm:ssZ`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserIrMetadata {
    pub name: String,
    pub date: String,
    pub cloud_id: String,
    pub coros_version: String,
}

impl UserIrMetadata {
    /// The stock serializer shape seen on device metadata: sorted keys,
    /// `"key": value`, `,` separators, no whitespace elsewhere (inferred for
    /// type 1 from type-2 rows).
    pub fn to_json(&self) -> String {
        let fields = [
            ("cloudId", &self.cloud_id),
            ("corosVersion", &self.coros_version),
            ("date", &self.date),
            ("name", &self.name),
        ];
        let body: Vec<String> = fields
            .iter()
            .map(|(key, value)| {
                format!(
                    "\"{key}\": {}",
                    serde_json::to_string(value).expect("string serializes")
                )
            })
            .collect();
        format!("{{{}}}", body.join(","))
    }

    /// Accepts any JSON object. `name` is required; other fields default to
    /// empty (owned policy).
    pub fn from_json(bytes: &[u8]) -> Result<Self, UserIrError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| UserIrError::MetadataJson)?;
        let object = value.as_object().ok_or(UserIrError::MetadataJson)?;
        let field = |key: &'static str, required: bool| match object.get(key) {
            Some(serde_json::Value::String(text)) => Ok(text.clone()),
            None if !required => Ok(String::new()),
            _ => Err(UserIrError::MetadataField(key)),
        };
        Ok(Self {
            name: field("name", true)?,
            date: field("date", false)?,
            cloud_id: field("cloudId", false)?,
            coros_version: field("corosVersion", false)?,
        })
    }

    /// Stock rows are encrypted. Plain JSON is also accepted on read.
    pub fn from_blob(blob: &[u8], cipher: &DeviceCipher) -> Result<Self, UserIrError> {
        Self::from_json(blob).or_else(|_| {
            let mut plain = blob.to_vec();
            cipher.apply(&mut plain);
            Self::from_json(&plain)
        })
    }

    pub fn to_blob(&self, cipher: &DeviceCipher) -> Vec<u8> {
        let mut blob = self.to_json().into_bytes();
        cipher.apply(&mut blob);
        blob
    }
}

/// One type-1 product row.
#[derive(Debug, Clone, PartialEq)]
pub struct UserIrProduct {
    pub id: String,
    pub samples: IrSamples,
    pub metadata: UserIrMetadata,
}

impl UserIrProduct {
    /// Stock import of a WAV payload. Stock requires a non-empty payload and a
    /// name; longer IRs are truncated to 1,024 samples. `policy` is explicit;
    /// `RecoveredHeader38` is the stock extractor layout.
    pub fn import_wav(
        wav: &[u8],
        policy: WavPolicy,
        limits: IrLimits,
        metadata: UserIrMetadata,
    ) -> Result<Self, UserIrError> {
        if metadata.name.is_empty() {
            return Err(UserIrError::EmptyName);
        }
        let samples = decode(wav, IrInputFormat::Wav(policy), limits)?;
        Ok(Self::new(samples, metadata))
    }

    /// Builds a row from already selected words (legacy `.bin` migration).
    pub fn new(samples: IrSamples, metadata: UserIrMetadata) -> Self {
        Self {
            id: product_id(&samples.to_le_bytes()),
            samples,
            metadata,
        }
    }

    /// Reads a stored row. The id is kept as stored, not recomputed.
    pub fn from_row(
        id: &str,
        rawdata: &[u8],
        metadata: &[u8],
        cipher: &DeviceCipher,
    ) -> Result<Self, UserIrError> {
        let limits = IrLimits {
            max_asset_bytes: MAX_IR_SAMPLES * 4,
            max_chunks: 0,
        };
        let samples =
            decode(rawdata, IrInputFormat::RawLeFloat32, limits).map_err(|error| match error {
                IrError::AssetLimit { actual, .. } => UserIrError::RawdataLength(actual / 4),
                other => other.into(),
            })?;
        Ok(Self {
            id: id.to_owned(),
            samples,
            metadata: UserIrMetadata::from_blob(metadata, cipher)?,
        })
    }

    pub fn rawdata(&self) -> Vec<u8> {
        self.samples.to_le_bytes()
    }
}

/// Qt `QString::simplified()`: trim, then collapse inner whitespace runs to one space.
pub fn simplify_name(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Rename dialog rule: simplified name of 4..=60 UTF-16 units. No character
/// whitelist was found on the device; cloud-side rules are separate.
pub fn dialog_name(input: &str) -> Result<String, UserIrError> {
    let name = simplify_name(input);
    let units = name.encode_utf16().count();
    if (MIN_IR_NAME_UNITS..=MAX_IR_NAME_UNITS).contains(&units) {
        Ok(name)
    } else {
        Err(UserIrError::NameLength(units))
    }
}

/// Names `nameExists` tries (VA 0x1d3bcc): as given, first letter upper-
/// cased, first letter lower-cased.
pub fn name_variants(name: &str) -> [String; 3] {
    let mut chars = name.chars();
    let (first, rest) = match chars.next() {
        Some(first) => (first, chars.as_str()),
        None => return [String::new(), String::new(), String::new()],
    };
    [
        name.to_owned(),
        first.to_uppercase().chain(rest.chars()).collect(),
        first.to_lowercase().chain(rest.chars()).collect(),
    ]
}

/// A preset IR parameter value (VA 0x326a0c / 0x3268b4 / 0x327d78).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrReference<'a> {
    Empty,
    /// A `CIR_` product id, or a legacy `CustomIR`/`…CustomIRs…` value.
    Custom(&'a str),
    /// `<tag>_<cab>_<mic>` factory selector.
    Factory {
        tag: &'a str,
        cab: &'a str,
        mic: &'a str,
    },
    /// Stock logs "Unexpected factory IR name".
    Unexpected(&'a str),
}

impl<'a> IrReference<'a> {
    pub fn classify(value: &'a str) -> Self {
        if value.is_empty() {
            return Self::Empty;
        }
        if value.starts_with(CUSTOM_IR_PREFIX) || value == "CustomIR" || value.contains("CustomIRs")
        {
            return Self::Custom(value);
        }
        let mut parts = value.split('_');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(tag), Some(cab), Some(mic), None) => Self::Factory { tag, cab, mic },
            _ => Self::Unexpected(value),
        }
    }
}

/// Selector label (VA 0x8221f8): the found product's name, else the stored
/// name parameter, else [`DELETED_IR_LABEL`].
pub fn display_name<'a>(found: Option<&'a str>, stored_name: &'a str) -> &'a str {
    match found {
        Some(name) => name,
        None if !stored_name.is_empty() => stored_name,
        None => DELETED_IR_LABEL,
    }
}

/// Parameters that hold an IR reference (id) and its display name for mic or
/// slot A and B (VA 0x515d58 ids, VA 0x515c08 names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrReferenceParams {
    pub id: [u32; 2],
    pub name: [u32; 2],
}

/// `category` is the ModelRepo category id; `base_model` is the model id after
/// resolving `clones` (the stock code tests ids directly; clone resolution is
/// inferred from the ModelRepo layouts).
pub fn reference_params(category: u32, base_model: u32) -> Option<IrReferenceParams> {
    const PARALLAX: u32 = 3008;
    let id = match (category, base_model) {
        (_, PARALLAX) => [15, 23],
        (12 | 21 | 32 | 33, _) => [1, 9],
        (29, _) => IR_PATH_PARAMS,
        _ => return None,
    };
    let name = match (category, base_model) {
        (_, PARALLAX) => [42, 43],
        (29, _) => IR_NAME_PARAMS,
        (_, 12100 | 32100) => [29, 30],
        (_, 12078 | 12115 | 12116 | 32089) => [26, 27],
        _ => [19, 20],
    };
    Some(IrReferenceParams { id, name })
}

/// One favorites/recents entry: a folder key and a product id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductRef {
    pub folder_key: String,
    pub product_id: String,
}

/// Parses `favorites-ir` / `recents-ir` (reader VA 0x69a8cc, loader VA
/// 0x7d0224): lines in pairs. A leftover line is "corrupt file". Dropping
/// entries whose product is gone is the caller's step.
pub fn parse_product_list(text: &str) -> Result<Vec<ProductRef>, UserIrError> {
    let lines: Vec<&str> = text.lines().collect();
    if !lines.len().is_multiple_of(2) {
        return Err(UserIrError::CorruptList);
    }
    Ok(lines
        .chunks_exact(2)
        .map(|pair| ProductRef {
            folder_key: pair[0].to_owned(),
            product_id: pair[1].to_owned(),
        })
        .collect())
}

/// Writer VA 0x69a800: every line followed by `\n`.
pub fn product_list_text(entries: &[ProductRef]) -> String {
    entries
        .iter()
        .flat_map(|entry| [entry.folder_key.as_str(), entry.product_id.as_str()])
        .fold(String::new(), |mut text, line| {
            text.push_str(line);
            text.push('\n');
            text
        })
}
