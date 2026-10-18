//! Cloud products: the two stock product record parsers, the product download, the payload forms of
//! the folder backends and the download queue.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};

use super::api::{ApiError, CloudApi};
use super::services::{ProductKind, path_segment, percent_encode};
use super::session::TokenStore;
use crate::backup::{self, BackupError};
use crate::http::{HttpClient, Method, Route};

impl ProductKind {
    /// The API `type` text (`0x71c3c4`): `preset` 0, `impulse_response` 1, `neural_capture`
    /// 2. Stock logs "bad product type" and reads anything else as a preset; Tetrad refuses it
    /// (O), as its payload would not be a preset.
    pub fn from_type(text: &str) -> Option<Self> {
        [Self::Preset, Self::ImpulseResponse, Self::NeuralCapture]
            .into_iter()
            .find(|kind| kind.as_str() == text)
    }

    /// Folder backend vtable `+0x9c`: the payload is an archive (presets `0x707b18` and
    /// captures `0x7bb7bc` return 1, IRs `0x727f4c` return 0).
    pub fn archive(self) -> bool {
        self != Self::ImpulseResponse
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProductError {
    #[error("bad product type \"{0}\"")]
    Type(String),
    #[error("Empty product id!")]
    EmptyId,
    #[error("got empty or non existing payload")]
    EmptyPayload,
    #[error("extracting tar payload failed: {0}")]
    Archive(String),
    #[error("a capture payload holds {0} file(s), not two")]
    CaptureFiles(usize),
    #[error("a product payload of {0} bytes is over the {PAYLOAD_LIMIT}-byte limit")]
    TooLarge(usize),
}

/// The largest product payload, before and after the archive is unpacked (O; review r26 S3):
/// the library's per-file read bound (`tetrad_library` `Limits::max_file_bytes`, 16 MiB). Stock
/// has no bound (`0x6b4d88` reads every member).
pub const PAYLOAD_LIMIT: u64 = 16 << 20;

/// The product record (`0x1ec070`) as a cloud parser fills it. Absent keys keep the record
/// defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudProduct {
    /// `+4`, copied to the cloud id `+0x30`.
    pub id: String,
    /// `+0x10`: `QFileInfo(name).baseName()` (the file part up to its first `.`).
    pub name: String,
    pub kind: ProductKind,
    pub author_username: String,
    pub author_id: String,
    /// `+0x1c` (`products/<id>` only).
    pub creator: String,
    pub creator_version: String,
    /// `+0x34` (`products/<id>` only).
    pub description: String,
    /// `+0x38` (search items only).
    pub shared_by: String,
    pub tags: Vec<String>,
    /// `+0x28` in UTC; `None` is a null `QDateTime` (absent or not in the stock form).
    pub date: Option<SystemTime>,
    /// `+0x2c`: -1 when absent; an integer value, else 0 (`QJsonValue::toInt(0)`).
    pub gain_type: i32,
    /// The device type name (`+0x60` through `0x7ec994`; an unknown name reads as code 0).
    pub device_type: String,
    /// The instrument type name (`+0x5c` through `0x7ec714`; unknown reads as code 0).
    pub instrument_type: String,
    /// `+0x40`, `"1"` by default.
    pub version: String,
    /// The whole `metadata` object (`+0x88`).
    pub metadata: Map<String, Value>,
    /// `dependencies.product` (`+0x50`, `0x71fce8`; `products/<id>` only), in reply order.
    pub dependencies: Vec<ProductDependency>,
}

/// One `dependencies.product` entry of a product record (`0x71fce8`, 0x14 bytes: `+0` hash,
/// `+4` id, `+8` name, `+0xc` product_type, `+0x10` missing, `+0x11` visible, `+0x12`
/// published). Texts are `toString("")`, flags `toBool(false)`; an entry that is not an
/// object is left out. The `dependencies.factory` hashes (`+0x44`) are not read: only the
/// pre-check dialogs of a single download use them (`0x742378`, not built).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProductDependency {
    pub hash: String,
    /// The dependency's cloud id.
    pub id: String,
    pub name: String,
    pub product_type: String,
    pub missing: bool,
    pub visible: bool,
    pub published: bool,
}

impl ProductDependency {
    /// A preset download fetches this dependency (`0x74012c`: not `missing`, `visible`, a
    /// non-empty id); it skips the others.
    pub fn fetched(&self) -> bool {
        !self.missing && self.visible && !self.id.is_empty()
    }
}

/// The key names of one parser.
struct Keys {
    id: &'static str,
    author_username: &'static str,
    author_id: &'static str,
    creator_version: &'static str,
    gain_type: &'static str,
    device_type: &'static str,
    instrument_type: &'static str,
}

impl CloudProduct {
    /// `0x722464`: one item of a search page (`0x72391c`), camelCase keys. The date is
    /// `sharedInfo[0].sharedAt` when `sharedInfo` is there (with `sharedBy`), else
    /// `orderingDate`, both `yyyy-MM-dd hh:mm:ss[.zzz]` (`0x716270`).
    pub fn from_search_item(item: &Value) -> Result<Self, ProductError> {
        let keys = Keys {
            id: "productId",
            author_username: "authorUsername",
            author_id: "authorId",
            creator_version: "creatorVersion",
            gain_type: "gainType",
            device_type: "deviceType",
            instrument_type: "instrumentType",
        };
        let (mut product, object) = Self::common(item, &keys)?;
        let date = |text: &str| parse_date(text, b' ', "");
        match object.get("sharedInfo") {
            None => product.date = date(&text(object, "orderingDate", "")),
            Some(shared) => {
                if let Some(Value::Object(first)) = shared.as_array().and_then(|list| list.first())
                {
                    if first.contains_key("sharedAt") {
                        product.date = date(&text(first, "sharedAt", ""));
                    }
                    if first.contains_key("sharedBy") {
                        product.shared_by = text(first, "sharedBy", "");
                    }
                }
            }
        }
        Ok(product)
    }

    /// `0x7240b0`: the `products/<id>` reply of a download (`0x724ce4`), snake_case keys,
    /// with `description`, `creator`, `created` (`yyyy-MM-ddThh:mm:ss[.zzz]Z`) and
    /// `dependencies.product` (`0x71fce8`, [`ProductDependency`]).
    pub fn from_product(reply: &Value) -> Result<Self, ProductError> {
        let keys = Keys {
            id: "id",
            author_username: "author_username",
            author_id: "author_id",
            creator_version: "creator_version",
            gain_type: "gain_type",
            device_type: "device_type",
            instrument_type: "instrument_type",
        };
        let (mut product, object) = Self::common(reply, &keys)?;
        product.description = text(object, "description", "");
        product.creator = text(object, "creator", "");
        product.date = parse_date(&text(object, "created", ""), b'T', "Z");
        let entries = (object.get("dependencies"))
            .and_then(|dependencies| dependencies.get("product"))
            .and_then(Value::as_array);
        product.dependencies = (entries.into_iter().flatten())
            .filter_map(Value::as_object)
            .map(|entry| {
                let flag = |key: &str| entry.get(key).and_then(Value::as_bool).unwrap_or(false);
                ProductDependency {
                    hash: text(entry, "hash", ""),
                    id: text(entry, "id", ""),
                    name: text(entry, "name", ""),
                    product_type: text(entry, "product_type", ""),
                    missing: flag("missing"),
                    visible: flag("visible"),
                    published: flag("published"),
                }
            })
            .collect();
        Ok(product)
    }

    fn common<'v>(
        value: &'v Value,
        keys: &Keys,
    ) -> Result<(Self, &'v Map<String, Value>), ProductError> {
        static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
        let object = value
            .as_object()
            .unwrap_or_else(|| EMPTY.get_or_init(Map::new));
        let kind_text = text(object, "type", "");
        let kind = ProductKind::from_type(&kind_text).ok_or(ProductError::Type(kind_text))?;
        let name = text(object, "name", "");
        let file = name.rsplit('/').next().unwrap_or_default();
        let mut product = Self {
            id: text(object, keys.id, ""),
            name: file.split('.').next().unwrap_or_default().to_owned(),
            kind,
            author_username: text(object, keys.author_username, ""),
            author_id: text(object, keys.author_id, ""),
            creator: String::new(),
            creator_version: text(object, keys.creator_version, ""),
            description: String::new(),
            shared_by: String::new(),
            tags: (object
                .get("tags")
                .and_then(Value::as_array)
                .into_iter()
                .flatten())
            .map(|tag| tag.as_str().unwrap_or_default().to_owned())
            .collect(),
            date: None,
            gain_type: -1,
            device_type: String::new(),
            instrument_type: String::new(),
            version: "1".to_owned(),
            metadata: Map::new(),
            dependencies: Vec::new(),
        };
        if let Some(metadata) = object.get("metadata") {
            let metadata = metadata.as_object().cloned().unwrap_or_default();
            if let Some(gain) = metadata.get(keys.gain_type) {
                product.gain_type = integer(gain).unwrap_or(0);
            }
            if metadata.contains_key(keys.device_type) {
                product.device_type = text(&metadata, keys.device_type, "");
            }
            if metadata.contains_key(keys.instrument_type) {
                product.instrument_type = text(&metadata, keys.instrument_type, "");
            }
            if metadata.contains_key("version") {
                product.version = text(&metadata, "version", "1");
            }
            product.metadata = metadata;
        }
        Ok((product, object))
    }
}

/// `QJsonValue::toString(default)`: a string, else `default`.
fn text(object: &Map<String, Value>, key: &str, default: &str) -> String {
    match object.get(key) {
        Some(Value::String(text)) => text.clone(),
        _ => default.to_owned(),
    }
}

/// `QJsonValue::toInt` (`0xe84e0`): a number with no fraction, in `i32`.
fn integer(value: &Value) -> Option<i32> {
    let number = value.as_f64()?;
    (number.fract() == 0.0 && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&number))
        .then_some(number as i32)
}

/// `QDateTime::fromString(text, "yyyy-MM-dd<sep>hh:mm:ss<suffix>")`, then the same with
/// `.zzz` before the suffix; `toTimeSpec(Qt::UTC)`. I: the unit's local time is UTC, so the
/// parsed local time is the UTC time. Milliseconds are dropped (every writer prints seconds).
pub fn parse_date(text: &str, separator: u8, suffix: &str) -> Option<SystemTime> {
    let body = text.strip_suffix(suffix)?.as_bytes();
    let main = match body.len() {
        19 => body,
        23 if body[19] == b'.' && body[20..].iter().all(u8::is_ascii_digit) => &body[..19],
        _ => return None,
    };
    let field = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = &main[range];
        digits.iter().all(u8::is_ascii_digit).then(|| {
            digits
                .iter()
                .fold(0, |value, digit| value * 10 + i64::from(digit - b'0'))
        })
    };
    let layout = [
        (4, b'-'),
        (7, b'-'),
        (10, separator),
        (13, b':'),
        (16, b':'),
    ];
    if layout.iter().any(|&(at, byte)| main[at] != byte) {
        return None;
    }
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return None,
    };
    if !(1..=days_in_month).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    let offset = Duration::from_secs(seconds.unsigned_abs());
    if seconds >= 0 {
        UNIX_EPOCH.checked_add(offset)
    } else {
        UNIX_EPOCH.checked_sub(offset)
    }
}

/// A product payload in the form its folder backend keeps (`0x71e820`, vtable `+0x9c`).
#[derive(Clone, PartialEq, Eq)]
pub enum ProductPayload {
    /// An IR (`+0x9c` = 0): the payload bytes (`0x71d538` with the payload alone).
    Raw(Vec<u8>),
    /// A preset (`0x71e4b0` → `0x6b5bc4`): the data of the first archive member.
    Preset(Vec<u8>),
    /// A capture (`0x71dd44` → `0x6b4d88`): `id` is the base name of the first member (it
    /// replaces the record id `+4`); `record` is the first member when its name ends in
    /// `.cns` (case-sensitive), else the second; `metadata` is the other one.
    Capture {
        id: String,
        record: Vec<u8>,
        metadata: Vec<u8>,
    },
}

impl fmt::Debug for ProductPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw(bytes) => write!(f, "Raw({} bytes)", bytes.len()),
            Self::Preset(bytes) => write!(f, "Preset({} bytes)", bytes.len()),
            Self::Capture {
                id,
                record,
                metadata,
            } => write!(
                f,
                "Capture({id}, {} + {} bytes)",
                record.len(),
                metadata.len()
            ),
        }
    }
}

impl ProductPayload {
    /// The payload bytes this form holds.
    pub fn size(&self) -> usize {
        match self {
            Self::Raw(bytes) | Self::Preset(bytes) => bytes.len(),
            Self::Capture {
                record, metadata, ..
            } => record.len() + metadata.len(),
        }
    }

    /// The downloaded payload (base64-decoded) in the form of `kind`. Stock reads the
    /// second member of a capture without a count check; Tetrad needs two (O). O: the payload
    /// and the unpacked members are bounded by [`PAYLOAD_LIMIT`], and only the members the
    /// form uses (one or two) are read.
    pub fn unpack(kind: ProductKind, payload: Vec<u8>) -> Result<Self, ProductError> {
        if payload.len() as u64 > PAYLOAD_LIMIT {
            return Err(ProductError::TooLarge(payload.len()));
        }
        if !kind.archive() {
            return Ok(Self::Raw(payload));
        }
        let archive = |error: BackupError| ProductError::Archive(error.to_string());
        let used = if kind == ProductKind::Preset { 1 } else { 2 };
        let mut files = (backup::unpack(&payload, used, PAYLOAD_LIMIT))
            .map_err(archive)?
            .into_iter();
        if kind == ProductKind::Preset {
            let (_, data) =
                (files.next()).ok_or_else(|| ProductError::Archive("no member".into()))?;
            return Ok(Self::Preset(data));
        }
        let count = files.len();
        let (Some((first_name, first)), Some((_, second))) = (files.next(), files.next()) else {
            return Err(ProductError::CaptureFiles(count));
        };
        let file = first_name.rsplit('/').next().unwrap_or_default();
        let id = file.split('.').next().unwrap_or_default().to_owned();
        let (record, metadata) = match first_name.ends_with(".cns") {
            true => (first, second),
            false => (second, first),
        };
        Ok(Self::Capture {
            id,
            record,
            metadata,
        })
    }

    /// The upload payload (`0x725ba4`): an IR as it is; a preset as a gzip pax tar of one
    /// `payload.pb` (`0x6b4c10` → `0x6b4b50`); a capture as a gzip pax tar of `<id>.cns` and
    /// `<id>.json` (`0x6b4c8c`).
    pub fn pack(&self) -> Result<Vec<u8>, BackupError> {
        match self {
            Self::Raw(bytes) => Ok(bytes.clone()),
            Self::Preset(bytes) => backup::pack(&[("payload.pb", bytes)]),
            Self::Capture {
                id,
                record,
                metadata,
            } => backup::pack(&[
                (&format!("{id}.cns"), record),
                (&format!("{id}.json"), metadata),
            ]),
        }
    }
}

/// `CaptureSearchResult` (`CloudNeuralCaptureFolder::searchForCapture` reply `0x7c09e8`): the
/// first item of the reply's `existing` array, or all empty when there is none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureSearch {
    /// `cloud_id` (`+0`).
    pub cloud_id: String,
    /// `hash` (`+4`).
    pub hash: String,
    /// `visible` (`+8`, `toBool(false)`).
    pub visible: bool,
    /// `author` (`+9`, `toBool(false)`): the account is the capture's author.
    pub author: bool,
}

impl CaptureSearch {
    /// The NC editor downloads a result with a cloud id that is visible or the account's own
    /// (`0x3c5ba4`); else it shows the missing-file text again (`0x3c5aa0`).
    pub fn downloadable(&self) -> bool {
        !self.cloud_id.is_empty() && (self.visible || self.author)
    }
}

impl CloudApi {
    /// `searchForCapture` (`0x7bcac8`): `GET products/exists?hash=<hash>` (30 s). A reply that
    /// is not a JSON object is "got invalid json response" (`0x7c09e8`). O: the hash is
    /// percent-encoded (stock appends it as it is; hashes are hex).
    pub fn search_capture(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        hash: &str,
        now: i64,
    ) -> Result<CaptureSearch, ApiError> {
        let url = (self.endpoint).api(&format!("products/exists?hash={}", percent_encode(hash)));
        let reply = self.call(http, store, Method::Get, url, Route::Product, now)?;
        let object = reply
            .as_object()
            .ok_or_else(|| ApiError::Reply("got invalid json response".into()))?;
        let Some(Value::Object(first)) = (object.get("existing").and_then(Value::as_array))
            .and_then(|existing| existing.first())
        else {
            return Ok(CaptureSearch::default());
        };
        let flag = |key: &str| first.get(key).and_then(Value::as_bool).unwrap_or(false);
        Ok(CaptureSearch {
            cloud_id: text(first, "cloud_id", ""),
            hash: text(first, "hash", ""),
            visible: flag("visible"),
            author: flag("author"),
        })
    }

    /// The upload check of one user capture (`0x6f94ec`). A record with another author: in
    /// [`CheckMode::Capture`] status 0 with no request; in [`CheckMode::Preset`] the hash
    /// search and `0x6f5324` (no id 0, `visible` 1, else 2). Else the hash search (`0x7bcac8`,
    /// reply `0x6f997c`): not found → the name check (`0x719794` → `0x6f52c4`: free 3, taken
    /// 4); found, the account not its author → `visible` 1, else 2; found, its author → the
    /// name check (`0x6f8fa8`: free 5, another id 6, the same id → `GET products/<id>`
    /// (`0x722068` → `0x721494`) and `0x6f7630`: name and tags, in order, equal 7, else 8).
    /// The name check asks for `neural_capture` (the cloud capture folder). Tetrad has no
    /// product cache, so 7/8 always fetch (I). Stock flag 1 (the capture's own upload) also
    /// saves the found cloud id into the local record before any upload (`0x6f997c` →
    /// `0x765520` `vt+0x50`); Tetrad changes nothing local before a good reply (O).
    pub fn capture_upload_status(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        check: &CaptureCheck,
        mode: CheckMode,
        now: i64,
    ) -> Result<CaptureUploadStatus, ApiError> {
        let account = self.claims().map(|claims| claims.user_id);
        let own = check.author_id.is_empty() || Some(&check.author_id) == account.as_ref();
        if !own && mode == CheckMode::Capture {
            return Ok(CaptureUploadStatus::NotAuthor);
        }
        let found = self.search_capture(http, store, &check.hash, now)?;
        if !found.cloud_id.is_empty() && (!own || !found.author) {
            return Ok(match found.visible {
                true => CaptureUploadStatus::Visible {
                    cloud_id: found.cloud_id,
                },
                false => CaptureUploadStatus::NotAuthor,
            });
        }
        if !own {
            return Ok(CaptureUploadStatus::NotAuthor);
        }
        let kind = ProductKind::NeuralCapture;
        let named = self.product_name_taken(http, store, &check.name, kind, now)?;
        let cloud_id = found.cloud_id;
        Ok(match (cloud_id.is_empty(), named) {
            (true, None) => CaptureUploadStatus::New,
            (true, Some(_)) => CaptureUploadStatus::NameTaken,
            (false, None) => CaptureUploadStatus::Ask { cloud_id },
            (false, Some(id)) if id != cloud_id => CaptureUploadStatus::NameTaken,
            (false, Some(_)) => {
                let reply = self.product(http, store, &cloud_id, now)?;
                let cloud = CloudProduct::from_product(&reply)
                    .map_err(|error| ApiError::Reply(error.to_string()))?;
                match cloud.name == check.name && cloud.tags == check.tags {
                    true => CaptureUploadStatus::Update { cloud_id },
                    false => CaptureUploadStatus::Ask { cloud_id },
                }
            }
        })
    }
}

/// What the upload check of a user capture reads from its record (`0x6f94ec`): the key, the
/// name (`+0x10`), the tags (`+0x24`) and `authorId` (`+0x18`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureCheck {
    pub hash: String,
    pub name: String,
    pub tags: Vec<String>,
    pub author_id: String,
}

/// Who asks for [`CloudApi::capture_upload_status`] (the last argument of `0x6f94ec`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    /// Mode 1: the capture's own upload from the Directory or Control (`0x1db0e0`).
    Capture,
    /// Mode 0: the capture walk of a preset upload (`0x74102c`).
    Preset,
}

/// The upload status of a user capture ([`CloudApi::capture_upload_status`]), grouped as the
/// status handlers act on it: the capture's own (`0x1e1fe0`) and the preset walk's
/// (`0x7419b8`). Stock numbers: r30-services §1 step 6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureUploadStatus {
    /// 0, 2: not the account's, and not found or not visible. The capture's own upload:
    /// "Uploading other users' Neural Captures is not permitted." (`0x464ce4`); the walk: "no
    /// longer available" (`+0xa8`).
    NotAuthor,
    /// 1: another author's capture, visible on the cloud as `cloud_id`. The capture's own
    /// upload refuses it as 0; the walk names it as a dependency (`+0xcc`).
    Visible { cloud_id: String },
    /// 3: not on the cloud and the name is free: a new product (POST).
    New,
    /// 4, 6: another product of the account has this name (`0x464e30` mode 0).
    NameTaken,
    /// 7: the account's capture with the same name and tags: an update (PUT) of `cloud_id`.
    Update { cloud_id: String },
    /// 5 (the account's copy has another name) and 8 (other tags): OVERWRITE updates
    /// `cloud_id`, CANCEL sends nothing (`0x465178` mode 0).
    Ask { cloud_id: String },
}

impl CloudApi {
    /// The product download (`0x720ebc` → `0x721abc` → `0x721494` → `0x724ce4`): `GET
    /// products/<id>/download` (30 s), whose JSON `payload` is base64; then `GET
    /// products/<id>` (30 s) through [`CloudProduct::from_product`]. Stock first looks in
    /// its product cache (`+0x30` vtable `+4`); Tetrad has none.
    pub fn download_cloud_product(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        id: &str,
        now: i64,
    ) -> Result<(CloudProduct, Vec<u8>), ApiError> {
        let failed = |error: ProductError| ApiError::Reply(error.to_string());
        if id.is_empty() {
            return Err(failed(ProductError::EmptyId));
        }
        let url = (self.endpoint).api(&format!("products/{}/download", path_segment(id)?));
        let reply = self.call(http, store, Method::Get, url, Route::Product, now)?;
        let payload = (reply.get("payload").and_then(Value::as_str))
            .and_then(|text| STANDARD.decode(text).ok())
            .filter(|payload| !payload.is_empty())
            .ok_or_else(|| failed(ProductError::EmptyPayload))?;
        let reply = self.product(http, store, id, now)?;
        Ok((CloudProduct::from_product(&reply).map_err(failed)?, payload))
    }

    /// Removes a stored download from the account's download queue (`0x1e4db4` →
    /// `0x718204` → `0x717c94`): `DELETE user/downloaded_products_queue/<cloud id>` (30 s;
    /// operation 5 is `QNetworkAccessManager::DeleteOperation`). I: `%1` of `user/%1/%2` is the
    /// queue name and `%2` the id (the `arg` calls are not in the Ghidra C).
    pub fn remove_from_download_queue(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        cloud_id: &str,
        now: i64,
    ) -> Result<(), ApiError> {
        let queued = path_segment(cloud_id)?;
        let url = (self.endpoint).api(&format!("user/downloaded_products_queue/{queued}"));
        self.call(http, store, Method::Delete, url, Route::Product, now)?;
        Ok(())
    }

    /// Removes a product from the user's cloud account: `DELETE user/products/<cloud id>`
    /// (`0x7180bc` → `0x717c94` with `"products"`). C: `0x7180bc` is word `+0x4c` (the folder
    /// remove) of the `CloudProductFolder` vtable `0x127ca70` and of its preset, IR and capture
    /// subclasses (`0x127c5e0`, `0x127cc90`, `0x1282ff0`), which Cortex Control DELETE reaches
    /// through `0x1d916c`. This deletes user data on the server: only an explicit request may
    /// call it.
    pub fn delete_product(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        cloud_id: &str,
        now: i64,
    ) -> Result<(), ApiError> {
        if cloud_id.is_empty() {
            return Err(ApiError::Reply(ProductError::EmptyId.to_string()));
        }
        let url = (self.endpoint).api(&format!("user/products/{}", path_segment(cloud_id)?));
        self.call(http, store, Method::Delete, url, Route::Product, now)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// `0x722464`: camelCase keys, the name's base name, the metadata codes as names, the
    /// shared date and `sharedBy`, non-text tags as "", and the record defaults for absent keys.
    #[test]
    fn a_search_item_fills_the_stock_record() {
        let item = json!({
            "productId": "p-1",
            "name": "dir/Crunch.v2.json",
            "type": "neural_capture",
            "authorUsername": "someone",
            "authorId": "a-1",
            "creatorVersion": "4.1.0",
            "metadata": {"gainType": 2.0, "deviceType": "amp_head", "version": "2"},
            "sharedInfo": [{"sharedAt": "2026-10-12 08:30:05.250", "sharedBy": "friend"}],
            "orderingDate": "2001-01-01 00:00:00",
            "tags": ["lead", 7],
        });
        let product = CloudProduct::from_search_item(&item).unwrap();
        assert_eq!(product.id, "p-1");
        assert_eq!(product.name, "Crunch");
        assert_eq!(product.kind, ProductKind::NeuralCapture);
        assert_eq!(product.gain_type, 2);
        assert_eq!(product.device_type, "amp_head");
        assert_eq!(product.instrument_type, "");
        assert_eq!(product.version, "2");
        assert_eq!(product.shared_by, "friend");
        assert_eq!(product.tags, ["lead", ""]);
        assert_eq!(
            product.date,
            Some(UNIX_EPOCH + Duration::from_secs(1_791_793_805))
        );

        let bare = CloudProduct::from_search_item(&json!({
            "type": "preset",
            "metadata": {"gainType": 1.5},
            "orderingDate": "2026-02-30 00:00:00",
        }))
        .unwrap();
        assert_eq!((bare.gain_type, bare.version.as_str()), (0, "1"));
        assert_eq!(bare.date, None, "an invalid date is a null QDateTime");
        assert!(matches!(
            CloudProduct::from_search_item(&json!({"type": "song"})),
            Err(ProductError::Type(_))
        ));
    }

    /// `0x7240b0`: snake_case keys, `description`, `creator` and the `created` form; a
    /// product without `metadata` keeps `gain_type` -1.
    #[test]
    fn a_product_reply_fills_the_stock_record() {
        let reply = json!({
            "id": "p-2",
            "name": "Clean",
            "type": "preset",
            "author_username": "someone",
            "author_id": "a-2",
            "creator_version": "4.1.0",
            "description": "bright",
            "creator": "quad",
            "created": "1970-01-02T00:00:01Z",
            "dependencies": {
                "factory": [{"hash": "f-1"}],
                "product": [
                    {"hash": "h-1", "id": "c-1", "name": "Amp", "product_type": "neural_capture",
                     "missing": false, "visible": true, "published": true},
                    "not an object",
                    {"hash": "h-2", "id": "c-2", "visible": "yes"},
                ],
            },
        });
        let product = CloudProduct::from_product(&reply).unwrap();
        assert_eq!(
            (product.id.as_str(), product.author_username.as_str()),
            ("p-2", "someone")
        );
        assert_eq!(
            (product.description.as_str(), product.creator.as_str()),
            ("bright", "quad")
        );
        assert_eq!(product.gain_type, -1);
        assert_eq!(product.date, Some(UNIX_EPOCH + Duration::from_secs(86_401)));
        // `0x71fce8`: object entries only; texts default to "", flags to false.
        let amp = ProductDependency {
            hash: "h-1".into(),
            id: "c-1".into(),
            name: "Amp".into(),
            product_type: "neural_capture".into(),
            missing: false,
            visible: true,
            published: true,
        };
        let hidden = ProductDependency {
            hash: "h-2".into(),
            id: "c-2".into(),
            ..ProductDependency::default()
        };
        assert_eq!(product.dependencies, [amp, hidden]);
        let fetched: Vec<bool> = product.dependencies.iter().map(|d| d.fetched()).collect();
        assert_eq!(fetched, [true, false]);
    }

    /// `+0x9c`: an IR stays raw; a preset is the first member; a capture names its record by
    /// `.cns` in either order and takes its id from the first member; the upload form reads
    /// back the same.
    #[test]
    fn payload_forms_follow_the_folder_backend() {
        assert_eq!(
            ProductPayload::unpack(ProductKind::ImpulseResponse, vec![1, 2]).unwrap(),
            ProductPayload::Raw(vec![1, 2])
        );
        let preset = ProductPayload::Preset(b"preset bytes".to_vec());
        assert_eq!(
            ProductPayload::unpack(ProductKind::Preset, preset.pack().unwrap()).unwrap(),
            preset
        );
        let reversed = backup::pack(&[("h1.json", b"meta"), ("h1.cns", b"record")]).unwrap();
        assert_eq!(
            ProductPayload::unpack(ProductKind::NeuralCapture, reversed).unwrap(),
            ProductPayload::Capture {
                id: "h1".into(),
                record: b"record".to_vec(),
                metadata: b"meta".to_vec(),
            }
        );
        let one = backup::pack(&[("h1.cns", b"record")]).unwrap();
        assert_eq!(
            ProductPayload::unpack(ProductKind::NeuralCapture, one),
            Err(ProductError::CaptureFiles(1))
        );
        let over = vec![0; PAYLOAD_LIMIT as usize + 1];
        assert_eq!(
            ProductPayload::unpack(ProductKind::ImpulseResponse, over),
            Err(ProductError::TooLarge(PAYLOAD_LIMIT as usize + 1))
        );
    }
}
