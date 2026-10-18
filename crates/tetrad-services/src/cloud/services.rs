//! Cloud endpoints on top of [`CloudApi`]: products, backups, logs, Neural Capture 2 jobs,
//! licences and hardware pairing.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::api::{ApiError, CloudApi, CloudError, exchange};
use super::endpoint::CloudEndpoint;
use super::session::TokenStore;
use super::signing::{SigningMaterial, Validation};
use super::{ClientSigner, DeviceIdentity, SignedRequest, sign_request, signing_message};
use crate::http::{BackupsOp, HttpClient, HttpRequest, LogsOp, Method, Route};

/// Product kinds as the API names them (VA 0x007169ac).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductKind {
    Preset,
    NeuralCapture,
    ImpulseResponse,
}

impl ProductKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preset => "preset",
            Self::NeuralCapture => "neural_capture",
            Self::ImpulseResponse => "impulse_response",
        }
    }
}

/// Which products a cloud folder lists (`0x716754`: folder `+0x40` == 1 lists the download
/// queue, else the account's own products).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseScope {
    OwnedByMe,
    DownloadQueue,
}

/// One page of a Directory cloud folder: `CloudFolderModel::fetchMore` (`0x433358`) asks folder
/// vtable `+0x38`, `CloudProductFolder::listCloudProducts` (`0x707f54` → `0x71aa28`), for the
/// rows `[n, n + size)` with `n` the rows listed so far, and sends `page` = `n / size` (B10,
/// 2026-10-16, C; the stock device logs show these requests). The other list,
/// `listCloudProductsById` (`0x718684`, `productId=<id>&after=&before=`), runs only from
/// fetchTop/fetchBottom around a model anchor, and the anchor is empty unless a view sets it
/// (`0x42fba0`); Tetrad does not use it, because the server refuses an empty `productId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowsePage {
    pub kind: ProductKind,
    pub scope: BrowseScope,
    /// Zero-based page number.
    pub page: u32,
    /// Rows per page (stock: 50).
    pub size: u32,
    /// Folder `+0x48`: the model's sort key `+0x58` through `0x74df0c` (default `0x21`:
    /// `name`). An empty key sends no sort.
    pub sort: String,
    /// Folder `+0x4c` (model `+0x5c`, default ascending).
    pub ascending: bool,
}

impl BrowsePage {
    /// `<cloud host>/search/v1/for/<type>?<scope>&searchOver=name&size=<n>&page=<n>
    /// &sort=<key>&sortDir=<dir>`, in stock order (`0x71aa28`); `<type>` is `preset`,
    /// `impulseResponse` or `neuralCapture` (`0x7167a0`), `<scope>` comes from `0x716754`. The
    /// cloud host is `0x45a43c`, without `/api/v1`. Not modelled: the `&query=` of a folder
    /// search (folder `+0x34`) and the preset filters (folder `+0x38`). O: the sort key is
    /// percent-encoded (stock appends it as it is; stock keys are plain words).
    pub fn url(&self, endpoint: &CloudEndpoint) -> String {
        let kind = match self.kind {
            ProductKind::Preset => "preset",
            ProductKind::ImpulseResponse => "impulseResponse",
            ProductKind::NeuralCapture => "neuralCapture",
        };
        let scope = match self.scope {
            BrowseScope::OwnedByMe => "ownedByMe=true",
            BrowseScope::DownloadQueue => "inDownloadedQueue=true",
        };
        let mut url = format!(
            "{}/search/v1/for/{kind}?{scope}&searchOver=name&size={}&page={}",
            endpoint.cloud_host(),
            self.size,
            self.page
        );
        if !self.sort.is_empty() {
            let direction = if self.ascending {
                "ascending"
            } else {
                "descending"
            };
            url += &format!("&sort={}&sortDir={direction}", percent_encode(&self.sort));
        }
        url
    }
}

/// A cloud product this upload depends on (`0x720ac4`, `dependencies.product`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductDependency {
    pub hash: String,
    pub id: String,
}

/// A Directory upload (`0x725ba4`; body `0x725020`, dependencies `0x720ac4`). The caller
/// packs the payload with [`super::products::ProductPayload::pack`] (folder vtable `+0x9c`):
/// a capture as `<id>.cns` and `<id>.json`, a preset as one `payload.pb`, an IR raw.
#[derive(Clone, PartialEq, Eq)]
pub struct ProductUpload {
    pub kind: ProductKind,
    /// The cloud id of an earlier upload: `PUT user/products/<id>`. `None` posts a new
    /// product (`POST user/products`).
    pub cloud_id: Option<String>,
    /// `id`: a new UUID without braces for each upload (`0x80a778`).
    pub request_id: String,
    pub name: String,
    pub author_username: String,
    /// Empty: the signed-in account's id (`0x2ff3f8` → `0x44d2fc`; I: the access token `sub`).
    pub author_id: String,
    /// The device type (`quad`, `atma`) when the product names none (`0x497ce0`).
    pub creator: String,
    pub creator_version: String,
    pub version: String,
    /// `created` and `updated`: one `yyyy-MM-ddThh:mm:ssZ` text.
    pub date: String,
    pub tags: Vec<String>,
    pub description: String,
    /// `preset_instrument_type`, sent for presets only (`0x7ec4c4`).
    pub instrument_type: Option<String>,
    pub dependencies: Vec<ProductDependency>,
    /// Hashes of the factory models the product uses (`dependencies.factory`).
    pub factory_dependencies: Vec<String>,
    /// The record's metadata object (`+0x88`): the body starts as a copy of it (`0xe5f00` at
    /// `0x725040`) and the stock keys then replace their own.
    pub metadata: serde_json::Map<String, Value>,
    pub payload: Vec<u8>,
    /// The product image as PNG.
    pub image: Option<Vec<u8>>,
    /// `X-Cloud-Api-Meta-Signature` before base64: for a capture the seed-only cipher over
    /// [`product_digest`] of its metadata (`0x6fffe4`), made by the holder of the cipher;
    /// empty for other kinds (stock sends the header empty).
    pub meta_signature: Vec<u8>,
    /// The `multipart/form-data` boundary of the Wi-Fi upload: a second new UUID without
    /// braces (`0x80ccf8` → `0x80a778`, `QHttpMultiPart::setBoundary`).
    pub boundary: String,
}

impl fmt::Debug for ProductUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProductUpload")
            .field("kind", &self.kind)
            .field("cloud_id", &self.cloud_id)
            .field("name", &self.name)
            .field("payload", &self.payload.len())
            .finish_non_exhaustive()
    }
}

/// Lowercase hex SHA-256 (`0x6a9204`): `payload_hash`, `image_hash` and the capture
/// meta-signature input.
pub fn product_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl ProductUpload {
    /// The JSON body of `0x725020`: the metadata object, then the stock keys. Both hashes are
    /// over the base64 text, as stock hashes the `toBase64` result (`0x725594` → `0x725620`,
    /// `0x7256d8` → `0x7257a8`). `created` and `updated` are the record date `+0x28`.
    fn body(&self, account_id: &str) -> Value {
        let payload = STANDARD.encode(&self.payload);
        let author_id = match self.author_id.is_empty() {
            true => account_id,
            false => &self.author_id,
        };
        let keys = json!({
            "id": self.request_id,
            "type": self.kind.as_str(),
            "name": self.name,
            "author_username": self.author_username,
            "creator_version": self.creator_version,
            "version": self.version,
            "author_id": author_id,
            "created": self.date,
            "updated": self.date,
            "tags": self.tags,
            "description": self.description,
            "creator": self.creator,
            "compatibility": ["quad", "native"],
            "genres": ["rock", "roll"],
            "payload_hash": product_digest(payload.as_bytes()),
            "payload": payload,
            "dependencies": {
                "product": self.dependencies.iter()
                    .map(|d| json!({ "hash": d.hash, "id": d.id }))
                    .collect::<Vec<_>>(),
                "factory": self.factory_dependencies.iter()
                    .map(|hash| json!({ "hash": hash }))
                    .collect::<Vec<_>>(),
            },
        });
        let mut body = Value::Object(self.metadata.clone());
        for (key, value) in keys.as_object().into_iter().flatten() {
            body[key] = value.clone();
        }
        if let (ProductKind::Preset, Some(instrument)) = (self.kind, &self.instrument_type) {
            body["preset_instrument_type"] = json!(instrument);
        }
        if let Some(image) = &self.image {
            let image = STANDARD.encode(image);
            body["image_type"] = json!("image/png");
            body["image_hash"] = json!(product_digest(image.as_bytes()));
            body["image"] = json!(image);
        }
        body
    }
}

/// Backup upload state from `GET <backup>/status` (VA 0x0030e5c0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupState {
    Completed,
    Pending,
    Missing,
    Other(String),
}

/// Presigned upload target from `generateBackupUrl` or `<backup>/update` (`0x30dce0`): the
/// `url` to PUT the backup file to and the backup's `metadata` ([`BackupData`]).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BackupUrl {
    pub url: String,
    pub metadata: Value,
}

impl BackupUrl {
    /// `metadata` as `0x306794` reads it. The create and update chains need it (`0x30dce0`
    /// with flag `+0x24`: "failed to find metadata"); its `backupUrl` is polled after the PUT.
    pub fn data(&self) -> Result<BackupData, ApiError> {
        BackupData::parse(&self.metadata)
    }
}

/// One backup record (`BackupData`, `0x306794`): an item of `GET /backups/v1` (`0x30eb7c`)
/// and the `metadata` of an upload target (`0x30dce0`). Every member must be there ("BackupData
/// source is missing member"); the key table is `0xa28b58`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupData {
    pub user_id: String,
    /// `quad_cortex` or `quad_cortex_mini` (the list keeps only these, `0x45bb04`).
    pub client_type: String,
    pub client_version: String,
    /// `backupName`, base64 in the reply (`QByteArray::fromBase64`).
    pub name: String,
    /// `backupUrl`: the backup's server path (`/backups/v1/…`).
    pub url: String,
    /// `backupKey`: the backup's identity (`0x3f58e4` matches an Update target by it).
    pub key: String,
    /// `timestamp`: Unix seconds (`QDateTime::fromTime_t`, then UTC).
    pub timestamp: i64,
}

impl BackupData {
    pub fn parse(item: &Value) -> Result<Self, ApiError> {
        let member = |key: &str| match item.get(key) {
            Some(value) => Ok(value.as_str().unwrap_or_default().to_owned()),
            None => Err(ApiError::Reply(format!(
                "BackupData source is missing member {key}"
            ))),
        };
        let name = member("backupName")?;
        // I: Qt's `fromBase64` skips characters outside the alphabet; the text ends at a NUL
        // (`QString::fromAscii`).
        let name: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/'))
            .collect();
        let name = base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(name)
            .unwrap_or_default();
        let name = name.split(|&byte| byte == 0).next().unwrap_or_default();
        let timestamp = item.get("timestamp").ok_or_else(|| {
            ApiError::Reply("BackupData source is missing member timestamp".into())
        })?;
        Ok(Self {
            user_id: member("userId")?,
            client_type: member("clientType")?,
            client_version: member("clientVersion")?,
            name: String::from_utf8_lossy(name).into_owned(),
            url: member("backupUrl")?,
            key: member("backupKey")?,
            // I: a value that is not a number reads as 0, as a Qt JSON number read does.
            timestamp: timestamp.as_f64().unwrap_or_default() as i64,
        })
    }
}

/// Upload targets from `generateLogUrl`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct LogUrls {
    #[serde(rename = "logUrl")]
    pub log_url: Option<String>,
    #[serde(rename = "coredumpUrl")]
    pub coredump_url: Option<String>,
}

/// NC2 training job (reply parser VA 0x00201118). `status` is `pending`, `pending_recording`,
/// `success` or `stopped`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureJob {
    pub id: Option<String>,
    pub status: String,
    pub progress: Option<f64>,
    pub status_reason: Option<String>,
    pub status_title: Option<String>,
    pub started_at: Option<String>,
    pub product_id: Option<String>,
    pub error: Option<Value>,
}

/// A checked licence reply's `activationCode`, decoded. The DSP gets it in request `0x50`.
/// Zeroed on drop; `Debug` shows the length only.
pub struct ActivationCode(Zeroizing<Vec<u8>>);

impl ActivationCode {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for ActivationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ActivationCode({} bytes)", self.0.len())
    }
}

/// One id or name as a URL path segment (review r32 E2, O): an empty one, one of dots only
/// (`.`, `..`: a proxy or the server may resolve them as dot segments, so `DELETE
/// user/products/..` could address a parent path) and one with a `/` are refused before any
/// request. Stock puts the id in as it is (`0x717c94`, `"user/%1/%2"`).
pub(super) fn path_segment(id: &str) -> Result<String, ApiError> {
    if id.bytes().all(|byte| byte == b'.') || id.contains('/') {
        return Err(ApiError::BadId(id.to_owned()));
    }
    Ok(percent_encode(id))
}

/// Percent-encode everything outside RFC 3986 unreserved characters.
pub(super) fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn parse<T: serde::de::DeserializeOwned>(reply: Value) -> Result<T, ApiError> {
    serde_json::from_value(reply).map_err(|e| ApiError::Reply(e.to_string()))
}

/// `0x30dce0`: a target needs a `url` ("failed to find upload URL") and a full `metadata`
/// record ("failed to find metadata").
fn backup_target(reply: Value) -> Result<BackupUrl, ApiError> {
    let target: BackupUrl = parse(reply)?;
    if target.url.is_empty() {
        return Err(ApiError::Reply("failed to find upload URL".into()));
    }
    target.data()?;
    Ok(target)
}

/// PUT the cloud backup file ([`crate::backup::local::encode`] without a stamp, `0x47a1c8`)
/// to its presigned `url`, no bearer. Wi-Fi (`via_control` `None`, `0x30b73c`): the
/// `<uuid>.json` file, `Content-Type: application/json` and `Content-Transfer-Encoding:
/// binary` (`0x80ccf8`), 180 s. Through Cortex Control (`Some(device)`, `0x1458ac`):
/// BackupsForward 30 CreateOrUpdate, 600 000 ms, the headers
/// `X-Cloud-Api-Hardware-Source-Type` and `-Version` of `device` and
/// `Content-Transfer-Encoding: binary` (C); the forward sends the body in chunks.
pub fn upload_backup(
    http: &mut impl HttpClient,
    target: &BackupUrl,
    via_control: Option<&DeviceIdentity>,
    file: Vec<u8>,
) -> Result<(), ApiError> {
    let route = Route::Backups(BackupsOp::CreateOrUpdate);
    let url = target.url.clone();
    let request = match via_control {
        None => HttpRequest::new(Method::Put, url, route, Duration::from_secs(180))
            .header("Content-Type", "application/json"),
        Some(device) => HttpRequest::new(Method::Put, url, route, Duration::from_secs(600))
            .header(
                "X-Cloud-Api-Hardware-Source-Type",
                device.client_type.as_str(),
            )
            .header(
                "X-Cloud-Api-Hardware-Source-Version",
                device.firmware.clone(),
            ),
    }
    .header("Content-Transfer-Encoding", "binary")
    .body(file);
    match http.send(&request)?.status {
        200..=299 => Ok(()),
        status => Err(ApiError::Cloud(CloudError::from_reply(status, b""))),
    }
}

/// PUT a gzip archive to a presigned log URL (VAs 0x006c0ab4, 0x006bd7cc): only
/// `Content-Type: application/gzip` and `Content-Encoding: gzip`, no bearer.
pub fn upload_log_archive(
    http: &mut impl HttpClient,
    url: &str,
    archive: Vec<u8>,
) -> Result<(), ApiError> {
    let request = HttpRequest::new(
        Method::Put,
        url,
        Route::Logs(LogsOp::UploadLogs),
        Duration::from_secs(300),
    )
    .header("Content-Type", "application/gzip")
    .header("Content-Encoding", "gzip")
    .body(archive);
    match http.send(&request)?.status {
        200..=299 => Ok(()),
        status => Err(ApiError::Cloud(CloudError::from_reply(status, b""))),
    }
}

/// The signed requests of [`CloudApi::signed_call`]. The update check has its own sender.
#[derive(Clone, Copy)]
enum SignedCall {
    Pair,
    Unpair,
    LicenceActivate,
}

impl CloudApi {
    /// `GET products/<id>` (VA 0x00721494).
    pub fn product(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        id: &str,
        now: i64,
    ) -> Result<Value, ApiError> {
        let url = (self.endpoint).api(&format!("products/{}", path_segment(id)?));
        self.call(http, store, Method::Get, url, Route::Product, now)
    }

    /// `GET products/<id>/download` (VA 0x00720ebc). The body is written to `dest`; its format
    /// is not recovered here.
    pub fn download_product(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        id: &str,
        dest: &Path,
        now: i64,
    ) -> Result<(), ApiError> {
        let url = (self.endpoint).api(&format!("products/{}/download", path_segment(id)?));
        self.ensure_fresh(http, store, now)?;
        let request = self.request(Method::Get, url, true, Route::Product);
        match http.download(&request, dest)? {
            200..=299 => Ok(()),
            status => Err(ApiError::Cloud(CloudError::from_reply(status, b""))),
        }
    }

    /// `GET products/<id>/download` for the NC2 product service (VA 0x007061b0): the body is
    /// the base64 payload (l-capture §V2), returned decoded and still encrypted. I: surrounding
    /// whitespace and JSON quotes are ignored.
    pub fn product_payload(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        id: &str,
        now: i64,
    ) -> Result<Vec<u8>, ApiError> {
        let url = (self.endpoint).api(&format!("products/{}/download", path_segment(id)?));
        self.ensure_fresh(http, store, now)?;
        let response = http.send(&self.request(Method::Get, url, true, Route::Product))?;
        if !response.is_success() {
            return Err(ApiError::Cloud(CloudError::from_reply(
                response.status,
                &response.body,
            )));
        }
        let text = response.body.trim_ascii();
        let text = text
            .strip_prefix(b"\"")
            .and_then(|t| t.strip_suffix(b"\""))
            .unwrap_or(text);
        STANDARD
            .decode(text)
            .map_err(|_| ApiError::Reply("product payload is not base64".into()))
    }

    /// `GET` of one cloud folder page ([`BrowsePage::url`], 30 s): the reply's `data` array,
    /// one cloud product object per item in list order (`0x723188`; each item goes through
    /// the product parser `0x722464`; its `numberOfResults` is not used). A reply without the
    /// array is an error ("got invalid json response").
    pub fn browse(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        page: &BrowsePage,
        now: i64,
    ) -> Result<Vec<Value>, ApiError> {
        let url = page.url(&self.endpoint);
        match self
            .call(http, store, Method::Get, url, Route::Product, now)?
            .get("data")
        {
            Some(Value::Array(items)) => Ok(items.clone()),
            _ => Err(ApiError::Reply(
                "cloud folder reply has no data array".into(),
            )),
        }
    }

    /// `GET user/products/exists?name=<n>&type=<kind>` (VA 0x00719794): the cloud id of the
    /// account's product of `kind` with this name, `None` when the name is free. The reply
    /// (`0x71f05c`): an object gives its `product_id` text (empty or not a text: free); any
    /// other reply is an error (`0x716a1c`).
    pub fn product_name_taken(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        name: &str,
        kind: ProductKind,
        now: i64,
    ) -> Result<Option<String>, ApiError> {
        let url = self.endpoint.api(&format!(
            "user/products/exists?name={}&type={}",
            percent_encode(name),
            kind.as_str()
        ));
        match self.call(http, store, Method::Get, url, Route::Product, now)? {
            Value::Object(reply) => Ok(reply
                .get("product_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)),
            _ => Err(ApiError::Reply(
                "the product name check reply is not an object".into(),
            )),
        }
    }

    /// A Directory upload (`0x725ba4`): `POST user/products` for a new product or
    /// `PUT user/products/<cloud id>` for an update, the JSON body of [`ProductUpload`] as the
    /// one `productFile` part of a multipart form ([`HttpRequest::multipart`]: the request sets
    /// the flag `+0x10` = 1, so it has no JSON Content-Type) and the
    /// `X-Cloud-Api-Meta-Signature` header. Returns the product's cloud id: the reply's `id`
    /// for a new product, the same id for an update (`0x71edc0`).
    pub fn upload_product(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        upload: &ProductUpload,
        now: i64,
    ) -> Result<String, ApiError> {
        let (method, path) = match &upload.cloud_id {
            Some(id) => (Method::Put, format!("user/products/{}", path_segment(id)?)),
            None => (Method::Post, "user/products".to_owned()),
        };
        self.ensure_fresh(http, store, now)?;
        let mut request = self
            .request(method, self.endpoint.api(&path), true, Route::Product)
            .header(
                "X-Cloud-Api-Meta-Signature",
                STANDARD.encode(&upload.meta_signature),
            )
            .body(
                upload
                    .body(&self.claims().map(|c| c.user_id).unwrap_or_default())
                    .to_string(),
            );
        request.headers.retain(|(name, _)| name != "Content-Type");
        request.multipart = Some(upload.boundary.clone());
        let reply = exchange(http, &request)?;
        match &upload.cloud_id {
            Some(id) => Ok(id.clone()),
            None => reply["id"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| ApiError::Reply("upload reply has no product id".into())),
        }
    }

    /// `GET user/downloaded_products/hash?type=<kind>` (VA 0x007820b0).
    pub fn downloaded_products_hash(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        kind: ProductKind,
        now: i64,
    ) -> Result<Value, ApiError> {
        let url = self.endpoint.api(&format!(
            "user/downloaded_products/hash?type={}",
            kind.as_str()
        ));
        self.call(http, store, Method::Get, url, Route::Product, now)
    }

    /// `GET /backups/v1`: the `backups` array (VAs 0x0030237c, 0x0030eb7c).
    pub fn list_backups(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        now: i64,
    ) -> Result<Vec<Value>, ApiError> {
        let url = self.endpoint.backups("");
        match self
            .call(http, store, Method::Get, url, Route::Product, now)?
            .get("backups")
        {
            Some(Value::Array(items)) => Ok(items.clone()),
            _ => Err(ApiError::Reply("no backups array".into())),
        }
    }

    /// `GET /backups/v1/generateBackupUrl/<name>` (VA 0x00303300).
    pub fn backup_upload_url(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        name: &str,
        now: i64,
    ) -> Result<BackupUrl, ApiError> {
        let segment = path_segment(name).map_err(|_| ApiError::BadBackupName(name.to_owned()))?;
        let url = self
            .endpoint
            .backups(&format!("/generateBackupUrl/{segment}"));
        backup_target(self.call(http, store, Method::Get, url, Route::Product, now)?)
    }

    /// Backup ⋮ Update: `GET <cloud host><backupUrl>/update` with the bearer and an empty body
    /// (`0x303564` → `0x45b7cc`, `0x302d50` method 2), the same reply as
    /// [`Self::backup_upload_url`]. The chain has no DELETE: the device never removes the old
    /// cloud backup (C).
    pub fn backup_update_url(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        path: &str,
        now: i64,
    ) -> Result<BackupUrl, ApiError> {
        let url = self.backup_url(path, "/update")?;
        backup_target(self.call(http, store, Method::Get, url, Route::Product, now)?)
    }

    /// Backup operations address one backup by its server path, which starts `/backups/v1/`
    /// (VAs 0x0030a83c status, 0x0030d054 rename, 0x003046a8 delete, 0x0030d4b0 download,
    /// 0x00303564 update).
    fn backup_url(&self, path: &str, suffix: &str) -> Result<String, ApiError> {
        if !path.starts_with("/backups/v1/") || path.contains("..") {
            return Err(ApiError::Reply("backup path outside /backups/v1/".into()));
        }
        Ok(format!("{}{path}{suffix}", self.endpoint.cloud_host()))
    }

    /// One `GET <cloud host><backupUrl>/status` with the bearer (`0x3084fc`, reply `0x30e5c0`:
    /// a missing `status` reads as `missing`). The caller polls it after the backup PUT.
    pub fn backup_state(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        path: &str,
        now: i64,
    ) -> Result<BackupState, ApiError> {
        let url = self.backup_url(path, "/status")?;
        let reply = self.call(http, store, Method::Get, url, Route::Product, now)?;
        Ok(
            match reply
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("missing")
            {
                "completed" => BackupState::Completed,
                "pending" => BackupState::Pending,
                "missing" => BackupState::Missing,
                other => BackupState::Other(other.to_owned()),
            },
        )
    }

    pub fn rename_backup(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        path: &str,
        name: &str,
        now: i64,
    ) -> Result<(), ApiError> {
        let url = self.backup_url(path, "/rename")?;
        self.ensure_fresh(http, store, now)?;
        let request = self
            .request(Method::Post, url, true, Route::Product)
            .body(json!({ "name": name }).to_string());
        exchange(http, &request).map(drop)
    }

    pub fn delete_backup(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        path: &str,
        now: i64,
    ) -> Result<(), ApiError> {
        let url = self.backup_url(path, "")?;
        self.call(http, store, Method::Delete, url, Route::Product, now)
            .map(drop)
    }

    pub fn backup_details(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        path: &str,
        now: i64,
    ) -> Result<Value, ApiError> {
        let url = self.backup_url(path, "")?;
        self.call(http, store, Method::Get, url, Route::Product, now)
    }

    /// `GET <cloud host>/capture/v1/jobs/<id>` (VA 0x0020068c). Job creation and cancel are not
    /// recovered.
    pub fn capture_job(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        job_id: &str,
        now: i64,
    ) -> Result<CaptureJob, ApiError> {
        if job_id.is_empty() {
            return Err(ApiError::Reply("training jobId is empty!".into()));
        }
        let url = self.endpoint.capture_job(&path_segment(job_id)?);
        parse(self.call(http, store, Method::Get, url, Route::Product, now)?)
    }

    /// `POST <cloud host>/purchase/v1/licenses/activate` with signed headers, the bearer and an
    /// empty body, 40 s (VAs 0x0045b008, 0x001eed80). Check the reply with
    /// [`Self::licence_activation_code`].
    pub fn activate_licenses(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        signer: &mut impl ClientSigner,
        now: i64,
    ) -> Result<Value, ApiError> {
        self.signed_call(http, store, signer, SignedCall::LicenceActivate, now)
    }

    /// Stock licence reply check (VA 0x001f2ccc): the server signs the device message with
    /// payload `activationCode` and its own `timestamp` (unix seconds), firmware prefix. A
    /// reply without the three keys is an error (stock drops it).
    pub fn verify_licence_reply(
        &self,
        material: &SigningMaterial,
        reply: &Value,
        now_ms: i64,
    ) -> Result<Validation, ApiError> {
        let missing =
            || ApiError::Reply("licence reply lacks activationCode/timestamp/signature".into());
        let code = reply
            .get("activationCode")
            .and_then(Value::as_str)
            .ok_or_else(missing)?;
        // Stock reads a JSON double and truncates it (`__aeabi_d2lz`).
        let timestamp = reply
            .get("timestamp")
            .and_then(Value::as_f64)
            .ok_or_else(missing)? as i64;
        let signature = reply
            .get("signature")
            .and_then(Value::as_str)
            .ok_or_else(missing)?;
        let message = signing_message(&self.device, self.device.firmware_prefix(), timestamp, code);
        Ok(material.validate(&message, signature, timestamp, now_ms))
    }

    /// The stock reply check, then `activationCode` decoded from base64 for DSP request `0x50`
    /// (VAs 0x001f2ccc, 0x001f2bf0, 0x00515640). A mismatch or stale reply is an error: stock
    /// ends the activation there (0x001f2714). Qt `fromBase64` skips bad characters; this
    /// decoder rejects them (owned).
    pub fn licence_activation_code(
        &self,
        material: &SigningMaterial,
        reply: &Value,
        now_ms: i64,
    ) -> Result<ActivationCode, ApiError> {
        match self.verify_licence_reply(material, reply, now_ms)? {
            Validation::Valid => {}
            Validation::Mismatch => {
                return Err(ApiError::Reply("licence reply signature mismatch".into()));
            }
            Validation::Stale => {
                return Err(ApiError::Reply("licence reply timestamp is stale".into()));
            }
        }
        let code = reply
            .get("activationCode")
            .and_then(Value::as_str)
            .unwrap_or_default();
        STANDARD
            .decode(code)
            .map(|bytes| ActivationCode(Zeroizing::new(bytes)))
            .map_err(|_| ApiError::Reply("licence activationCode is not base64".into()))
    }

    /// `POST` (pair) or `DELETE` (unpair) `<cloud host>/hardware/v1/pair` with signed headers
    /// and the bearer (VAs 0x0045b2a4, 0x00301ec0).
    pub fn pair_hardware(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        signer: &mut impl ClientSigner,
        pair: bool,
        now: i64,
    ) -> Result<Value, ApiError> {
        let call = if pair {
            SignedCall::Pair
        } else {
            SignedCall::Unpair
        };
        self.signed_call(http, store, signer, call, now)
    }

    fn signed_call(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        signer: &mut impl ClientSigner,
        call: SignedCall,
        now: i64,
    ) -> Result<Value, ApiError> {
        let (method, kind, path) = match call {
            SignedCall::Pair => (Method::Post, SignedRequest::Pair, "/hardware/v1/pair"),
            SignedCall::Unpair => (Method::Delete, SignedRequest::Pair, "/hardware/v1/pair"),
            SignedCall::LicenceActivate => (
                Method::Post,
                SignedRequest::LicenceActivate,
                "/purchase/v1/licenses/activate",
            ),
        };
        // Auth flag +0x18 stays 1 (0x00310a74, 0x001eed80): refresh first (0x0080da7c), then
        // the bearer (0x0044d714). Flag +0x28 only forwards these headers through Control.
        self.ensure_fresh(http, store, now)?;
        let url = format!("{}{path}", self.endpoint.cloud_host());
        let mut request = self.request(method, url, true, Route::Product);
        // The body at +0x08 is never written: POST sends it empty, DELETE has none (0x0080ccf8).
        if method == Method::Post {
            request = request.body(Vec::new());
        }
        request.timeout = Duration::from_millis(40_000);
        sign_request(&mut request, signer, &self.device, kind, now)?;
        exchange(http, &request)
    }

    /// `POST <logs host>/logs/v1/generateLogUrl` (VAs 0x006bcb30, 0x00182868). `api_key` is the
    /// per-environment `X-Api-Key`; stock embeds it obfuscated, here it is injected. No bearer.
    pub fn log_upload_urls(
        &self,
        http: &mut impl HttpClient,
        api_key: &str,
        email: &str,
        correlation_id: &str,
        coredump: bool,
    ) -> Result<LogUrls, ApiError> {
        let url = self
            .endpoint
            .log_url_request()
            .ok_or(ApiError::NoHost("log upload"))?;
        let body = json!({
            "cortexSerial": self.device.serial,
            "email": email,
            "corelationId": correlation_id,
            "coredump": coredump,
        });
        let op = if coredump {
            LogsOp::UploadCoredump
        } else {
            LogsOp::FetchUrls
        };
        let request = HttpRequest::new(
            Method::Post,
            url,
            Route::Logs(op),
            super::api::DEFAULT_TIMEOUT,
        )
        .header("Content-Type", "application/json")
        .header("X-Api-Key", api_key)
        .header(
            "X-Cloud-Api-Hardware-Source-Type",
            self.device.client_type.as_str(),
        )
        .header(
            "X-Cloud-Api-Hardware-Source-Version",
            self.device.firmware.clone(),
        )
        .body(body.to_string());
        parse(exchange(http, &request)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::SignError;
    use crate::cloud::api::tests::api;
    use crate::cloud::session::tests::jwt;
    use crate::fake::{MemTokens, ScriptedHttp};

    struct FixedSigner;

    impl ClientSigner for FixedSigner {
        fn sign(&mut self, message: &str) -> Result<String, SignError> {
            Ok(format!("sig({message})"))
        }
        fn client_id(&mut self) -> Result<String, SignError> {
            Ok("Y2xpZW50".into())
        }
    }

    fn signed_in() -> (CloudApi, ScriptedHttp, MemTokens) {
        let (mut api, mut http, mut store) = api();
        http.respond(
            200,
            json!({
                "access_token": jwt(r#"{"sub":"u","exp":2000000000,"iat":1999996400}"#),
                "refresh_token": "r",
            })
            .to_string(),
        );
        api.sign_in(&mut http, &mut store, "a", "b").unwrap();
        (api, http, store)
    }

    /// `0x71aa28` (B10): a cloud folder page asks by page and size with the stock query order
    /// on the cloud host (no `/api/v1`), never with a `productId`, and returns the `data`
    /// items; no array is an error. The stock device log has this exact preset request.
    #[test]
    fn a_cloud_folder_page_asks_by_page_and_size() {
        let (mut api, mut http, mut store) = signed_in();
        let mut page = BrowsePage {
            kind: ProductKind::Preset,
            scope: BrowseScope::OwnedByMe,
            page: 0,
            size: 50,
            sort: "name".into(),
            ascending: true,
        };
        http.respond(200, r#"{"data":[{"id":"a"},{"id":"b"}]}"#);
        let items = api
            .browse(&mut http, &mut store, &page, 1_999_997_000)
            .unwrap();
        assert_eq!(items, [json!({"id": "a"}), json!({"id": "b"})]);
        let host = api.endpoint.cloud_host();
        assert_eq!(
            http.requests[1].url,
            format!(
                "{host}/search/v1/for/preset?ownedByMe=true&searchOver=name&size=50&page=0\
                 &sort=name&sortDir=ascending"
            )
        );
        page.kind = ProductKind::NeuralCapture;
        page.scope = BrowseScope::DownloadQueue;
        page.page = 2;
        page.sort.clear();
        assert_eq!(
            page.url(&api.endpoint),
            format!(
                "{host}/search/v1/for/neuralCapture?inDownloadedQueue=true&searchOver=name&size=50&page=2"
            )
        );
        http.respond(200, "{}");
        assert!(
            api.browse(&mut http, &mut store, &page, 1_999_997_000)
                .is_err()
        );
    }

    /// `0x725ba4`: a new capture posts with its meta-signature header; the body carries the
    /// stock fixed fields and the hash of the base64 payload, on top of the record metadata
    /// (`0x725020`); the reply's `id` is the cloud id. An update puts to that id; an empty
    /// author id is the account's (`0x2ff3f8`).
    #[test]
    fn product_upload_posts_then_puts() {
        let (mut api, mut http, mut store) = signed_in();
        let mut upload = ProductUpload {
            kind: ProductKind::NeuralCapture,
            cloud_id: None,
            request_id: "0f0e0d0c-0000-4000-8000-000000000001".into(),
            name: "My Amp".into(),
            author_username: "user".into(),
            author_id: "u-1".into(),
            creator: "quad".into(),
            creator_version: "4.1.0".into(),
            version: "1".into(),
            date: "2026-10-12T10:00:00Z".into(),
            tags: vec!["clean".into()],
            description: String::new(),
            instrument_type: Some("guitar".into()),
            dependencies: Vec::new(),
            factory_dependencies: vec!["f-hash".into()],
            payload: crate::backup::pack(&[("c1.cns", b"capture"), ("c1.json", b"{}")]).unwrap(),
            image: None,
            meta_signature: b"made-up".to_vec(),
            boundary: "b-0001".into(),
            metadata: json!({"gainType": 2, "name": "old"})
                .as_object()
                .unwrap()
                .clone(),
        };
        http.respond(200, r#"{"id":"cloud-7"}"#);
        let id = api
            .upload_product(&mut http, &mut store, &upload, 0)
            .unwrap();
        assert_eq!(id, "cloud-7");
        let sent = &http.requests[1];
        assert_eq!(sent.method, Method::Post);
        assert_eq!(
            sent.url,
            "https://cloud-api.neuraldsp.com/api/v1/user/products"
        );
        assert_eq!(
            sent.header_value("X-Cloud-Api-Meta-Signature"),
            Some(STANDARD.encode(b"made-up").as_str())
        );
        // U1: the JSON is the one `productFile` part; the client sets the form's Content-Type.
        assert_eq!(sent.multipart.as_deref(), Some("b-0001"));
        assert_eq!(sent.header_value("Content-Type"), None);
        let body: Value = serde_json::from_slice(sent.body.as_deref().unwrap()).unwrap();
        let payload = body["payload"].as_str().unwrap();
        assert_eq!(body["payload_hash"], product_digest(payload.as_bytes()));
        assert_eq!(body["type"], "neural_capture");
        assert_eq!(body["compatibility"], json!(["quad", "native"]));
        assert_eq!(body["genres"], json!(["rock", "roll"]));
        assert_eq!(body["created"], body["updated"]);
        assert_eq!(
            body["dependencies"]["factory"],
            json!([{ "hash": "f-hash" }])
        );
        assert!(body.get("preset_instrument_type").is_none(), "presets only");
        assert!(body.get("image").is_none());
        assert_eq!(
            (&body["gainType"], &body["name"], &body["author_id"]),
            (&json!(2), &json!("My Amp"), &json!("u-1")),
            "the metadata is the base, the stock keys replace their own"
        );

        upload.cloud_id = Some("cloud-7".into());
        upload.author_id = String::new();
        http.respond(200, "{}");
        let id = api
            .upload_product(&mut http, &mut store, &upload, 0)
            .unwrap();
        assert_eq!(id, "cloud-7");
        assert_eq!(http.requests[2].method, Method::Put);
        assert!(http.requests[2].url.ends_with("/user/products/cloud-7"));
        let body: Value =
            serde_json::from_slice(http.requests[2].body.as_deref().unwrap()).unwrap();
        assert_eq!(
            body["author_id"], "u",
            "an empty author id is the account's"
        );
    }

    /// `0x6f94ec` mode 1 (`0x1db0e0`): another author is refused before any request; else
    /// the hash search, the `neural_capture` name check and, for the account's capture under
    /// the same id, its cloud record (name and tags in order: 7 update, else 8 ask).
    #[test]
    fn a_capture_upload_status_follows_the_stock_table() {
        use crate::cloud::products::{CaptureCheck, CaptureUploadStatus as S, CheckMode};
        let (mut api, mut http, mut store) = signed_in();
        let check = CaptureCheck {
            hash: "h1".into(),
            name: "Amp".into(),
            tags: vec!["clean".into(), "lead".into()],
            author_id: String::new(),
        };
        let mut status = |author_id: &str, replies: &[&str]| {
            for reply in replies {
                http.respond(200, *reply);
            }
            let sent = http.requests.len();
            let check = CaptureCheck {
                author_id: author_id.into(),
                ..check.clone()
            };
            let mode = CheckMode::Capture;
            let status = api.capture_upload_status(&mut http, &mut store, &check, mode, 0);
            let urls: Vec<String> = (http.requests[sent..].iter())
                .map(|request| request.url.rsplit("/api/v1/").next().unwrap().to_owned())
                .collect();
            (status.unwrap(), urls)
        };
        let (none, mine) = (
            r#"{"existing":[]}"#,
            r#"{"existing":[{"cloud_id":"c-1","author":true}]}"#,
        );
        let product = |tags: &str| {
            format!(r#"{{"id":"c-1","type":"neural_capture","name":"Amp","tags":{tags}}}"#)
        };
        assert_eq!(status("other", &[]), (S::NotAuthor, vec![]), "status 0");
        let (new, urls) = status("u", &[none, r#"{"product_id":""}"#]);
        assert_eq!(new, S::New, "status 3");
        assert_eq!(
            urls,
            [
                "products/exists?hash=h1",
                "user/products/exists?name=Amp&type=neural_capture"
            ]
        );
        assert_eq!(
            status("", &[none, r#"{"product_id":"x"}"#]).0,
            S::NameTaken,
            "4"
        );
        let other = r#"{"existing":[{"cloud_id":"c-1","visible":true}]}"#;
        let visible = S::Visible {
            cloud_id: "c-1".into(),
        };
        assert_eq!(
            status("", &[other]),
            (visible, vec!["products/exists?hash=h1".into()]),
            "1"
        );
        let ask = S::Ask {
            cloud_id: "c-1".into(),
        };
        assert_eq!(
            status("", &[mine, r#"{"product_id":""}"#]).0,
            ask,
            "status 5"
        );
        assert_eq!(
            status("", &[mine, r#"{"product_id":"c-2"}"#]).0,
            S::NameTaken,
            "6"
        );
        let same = product(r#"["clean","lead"]"#);
        let (update, urls) = status("", &[mine, r#"{"product_id":"c-1"}"#, &same]);
        assert_eq!(
            update,
            S::Update {
                cloud_id: "c-1".into()
            },
            "status 7"
        );
        assert_eq!(urls.last().map(String::as_str), Some("products/c-1"));
        let reordered = product(r#"["lead","clean"]"#);
        assert_eq!(
            status("", &[mine, r#"{"product_id":"c-1"}"#, &reordered]).0,
            ask,
            "8"
        );
    }

    /// `0x6f94ec` mode 0 (the preset walk `0x74102c`): a record of another author is looked
    /// up by hash (`0x6f5324`): no id 0, `visible` 1 with the found id, else 2. No name check.
    #[test]
    fn the_preset_walk_status_looks_up_another_authors_capture() {
        use crate::cloud::products::{CaptureCheck, CaptureUploadStatus as S, CheckMode};
        let (mut api, mut http, mut store) = signed_in();
        let check = CaptureCheck {
            hash: "h1".into(),
            name: "Amp".into(),
            tags: Vec::new(),
            author_id: "someone-else".into(),
        };
        let mut status = |reply: &str| {
            http.respond(200, reply);
            let mode = CheckMode::Preset;
            let sent = http.requests.len();
            let status = api.capture_upload_status(&mut http, &mut store, &check, mode, 0);
            (status.unwrap(), http.requests.len() - sent)
        };
        assert_eq!(status(r#"{"existing":[]}"#), (S::NotAuthor, 1), "0");
        let hidden = r#"{"existing":[{"cloud_id":"c-1","visible":false}]}"#;
        assert_eq!(status(hidden), (S::NotAuthor, 1), "2");
        let shown = r#"{"existing":[{"cloud_id":"c-1","visible":true}]}"#;
        let visible = S::Visible {
            cloud_id: "c-1".into(),
        };
        assert_eq!(status(shown), (visible, 1), "1");
    }

    /// `0x719794` → `0x71f05c`: the name check asks with the stock query and takes the
    /// reply's `product_id`; an empty id is a free name, a reply that is not an object fails.
    #[test]
    fn the_product_name_check_takes_the_product_id() {
        let (mut api, mut http, mut store) = signed_in();
        http.respond(200, r#"{"product_id":"cloud-3"}"#);
        http.respond(200, r#"{"product_id":""}"#);
        http.respond(200, "[]");
        let mut check =
            || api.product_name_taken(&mut http, &mut store, "Lead & Co", ProductKind::Preset, 0);
        assert_eq!(check().unwrap().as_deref(), Some("cloud-3"));
        assert_eq!(check().unwrap(), None);
        assert!(matches!(check(), Err(ApiError::Reply(_))));
        assert!(
            http.requests[1]
                .url
                .ends_with("/user/products/exists?name=Lead%20%26%20Co&type=preset"),
            "{}",
            http.requests[1].url
        );
    }

    /// `0x7180bc`: one `DELETE user/products/<id>` for exactly the given id (percent-encoded);
    /// an empty id sends nothing.
    #[test]
    fn a_product_delete_sends_one_delete_for_its_id() {
        let (mut api, mut http, mut store) = signed_in();
        http.respond(200, "{}");
        api.delete_product(&mut http, &mut store, "cloud 7", 0)
            .unwrap();
        assert_eq!(http.requests.len(), 2, "the sign-in, then one request");
        assert_eq!(http.requests[1].method, Method::Delete);
        assert_eq!(
            http.requests[1].url,
            "https://cloud-api.neuraldsp.com/api/v1/user/products/cloud%207"
        );
        assert!(api.delete_product(&mut http, &mut store, "", 0).is_err());
        assert_eq!(http.requests.len(), 2);
        // Review r32 E2: no id may address a parent path, in any `<route>/<id>` builder.
        for id in [".", "..", "...", "a/b", "/"] {
            let bad = |result: Result<_, ApiError>| matches!(result, Err(ApiError::BadId(_)));
            assert!(
                bad(api.delete_product(&mut http, &mut store, id, 0)),
                "{id}"
            );
            let queue = api.remove_from_download_queue(&mut http, &mut store, id, 0);
            assert!(bad(queue), "{id}");
            let download = api.download_cloud_product(&mut http, &mut store, id, 0);
            assert!(bad(download.map(drop)), "{id}");
        }
        assert_eq!(http.requests.len(), 2, "no request for a refused id");
    }

    /// A `BackupData` object (`0x306794`) with `backupName` "My Rig" in base64.
    const BACKUP_DATA: &str = r#"{"userId":"u1","clientType":"quad_cortex","clientVersion":"4.1.0","backupName":"TXkgUmln","backupUrl":"/backups/v1/u/abc","backupKey":"k1","timestamp":1756487160}"#;

    #[test]
    fn backup_paths_and_states() {
        let (mut api, mut http, mut store) = signed_in();
        http.respond(
            200,
            format!(r#"{{"url":"https://bucket.example/put","metadata":{BACKUP_DATA}}}"#),
        );
        let target = api
            .backup_upload_url(&mut http, &mut store, "My Rig", 0)
            .unwrap();
        assert_eq!(
            http.requests[1].url,
            "https://cloud-api.neuraldsp.com/backups/v1/generateBackupUrl/My%20Rig"
        );
        assert_eq!(target.url, "https://bucket.example/put");
        let data = target.data().unwrap();
        assert_eq!(
            (data.name.as_str(), data.url.as_str(), data.key.as_str()),
            ("My Rig", "/backups/v1/u/abc", "k1")
        );
        assert_eq!(data.timestamp, 1_756_487_160);
        // Review r33 F6: a backup name that cannot be a path segment has its own text.
        let sent_before = http.requests.len();
        assert!(matches!(
            api.backup_upload_url(&mut http, &mut store, "a/b", 0),
            Err(ApiError::BadBackupName(name)) if name == "a/b"
        ));
        assert_eq!(http.requests.len(), sent_before);
        // `0x30dce0`: a target without a full `metadata` record is a failure.
        http.respond(200, r#"{"url":"https://bucket.example/put","metadata":{}}"#);
        assert!(matches!(
            api.backup_upload_url(&mut http, &mut store, "My Rig", 0),
            Err(ApiError::Reply(reason)) if reason.contains("missing member")
        ));

        // Update (`0x303564`): one authorised GET of `<backupUrl>/update`, no body.
        http.respond(
            200,
            format!(r#"{{"url":"https://bucket.example/put2","metadata":{BACKUP_DATA}}}"#),
        );
        let update = api
            .backup_update_url(&mut http, &mut store, "/backups/v1/u/abc", 0)
            .unwrap();
        let sent = http.requests.last().unwrap();
        assert_eq!(sent.method, Method::Get);
        assert_eq!(
            sent.url,
            "https://cloud-api.neuraldsp.com/backups/v1/u/abc/update"
        );
        assert!(sent.body.is_none());
        assert!(sent.headers.iter().any(|(name, _)| name == "Authorization"));
        assert_eq!(update.url, "https://bucket.example/put2");
        let sent_before = http.requests.len();
        assert!(
            api.backup_update_url(&mut http, &mut store, "/etc/x", 0)
                .is_err()
        );
        assert_eq!(http.requests.len(), sent_before, "a bad path sends nothing");

        // `0x30b73c`: the backup file goes as JSON, without the bearer, 180 s.
        http.respond(200, "");
        upload_backup(&mut http, &update, None, b"{}".to_vec()).unwrap();
        let put = http.requests.last().unwrap();
        assert_eq!(
            (put.method, put.url.as_str()),
            (Method::Put, "https://bucket.example/put2")
        );
        let header = |key: &str| {
            (put.headers.iter())
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("Content-Type"), Some("application/json"));
        assert_eq!(header("Authorization"), None);
        assert_eq!(put.timeout, Duration::from_secs(180));

        http.respond(200, r#"{"status":"pending"}"#);
        assert_eq!(
            api.backup_state(&mut http, &mut store, "/backups/v1/u/abc", 0)
                .unwrap(),
            BackupState::Pending
        );
        assert!(
            http.requests
                .last()
                .unwrap()
                .url
                .ends_with("/backups/v1/u/abc/status")
        );
        assert!(matches!(
            api.delete_backup(&mut http, &mut store, "/etc/passwd", 0),
            Err(ApiError::Reply(_))
        ));
    }

    /// re-r44 §3 (C `0x80da7c`, `0x140824` → `0x30d054`, `0x30d4b0` → `0x302d50`): every JSON
    /// backup call is a JsonApiAccess call, so over Cortex Control it is ProductForward 29;
    /// only the body transfer is BackupsForward 30 (`0x148e54`).
    #[test]
    fn backup_json_calls_go_as_product_forward_29() {
        let (mut api, mut http, mut store) = signed_in();
        let target = format!(r#"{{"url":"https://bucket.example/put","metadata":{BACKUP_DATA}}}"#);
        let path = "/backups/v1/u/abc";
        for reply in [
            r#"{"backups":[]}"#,
            &target,
            &target,
            r#"{"status":"completed"}"#,
        ] {
            http.respond(200, reply);
        }
        for reply in ["{}", "{}", r#"{"url":"https://bucket.example/get"}"#, ""] {
            http.respond(200, reply);
        }
        api.list_backups(&mut http, &mut store, 0).unwrap();
        let update = (api.backup_upload_url(&mut http, &mut store, "Rig", 0))
            .and_then(|_| api.backup_update_url(&mut http, &mut store, path, 0))
            .unwrap();
        api.backup_state(&mut http, &mut store, path, 0).unwrap();
        api.rename_backup(&mut http, &mut store, path, "Rig 2", 0)
            .unwrap();
        api.delete_backup(&mut http, &mut store, path, 0).unwrap();
        api.backup_details(&mut http, &mut store, path, 0).unwrap();
        upload_backup(&mut http, &update, None, b"{}".to_vec()).unwrap();
        let routes: Vec<_> = http.requests[1..].iter().map(|sent| sent.route).collect();
        let mut expected = vec![Route::Product; 7];
        expected.push(Route::Backups(BackupsOp::CreateOrUpdate));
        assert_eq!(routes, expected);

        // `0x1458ac`: the body over Control has the Hardware-Source headers and 600 000 ms.
        http.respond(200, "");
        upload_backup(&mut http, &update, Some(&api.device), b"{}".to_vec()).unwrap();
        let put = http.requests.last().unwrap();
        let headers: Vec<_> = (put.headers.iter())
            .map(|(name, value)| format!("{name}: {value}"))
            .collect();
        assert_eq!(
            headers,
            [
                "X-Cloud-Api-Hardware-Source-Type: quad_cortex",
                "X-Cloud-Api-Hardware-Source-Version: 4.1.0",
                "Content-Transfer-Encoding: binary",
            ]
        );
        assert_eq!(
            (put.route, put.timeout),
            (expected[7], Duration::from_millis(600_000))
        );
    }

    #[test]
    fn licence_request_carries_signed_headers() {
        let (mut api, mut http, mut store) = signed_in();
        http.respond(200, r#"{"payload":"x"}"#);
        api.activate_licenses(&mut http, &mut store, &mut FixedSigner, 1_700_000_000)
            .unwrap();
        let sent = &http.requests[1];
        assert_eq!(
            sent.url,
            "https://cloud-api.neuraldsp.com/purchase/v1/licenses/activate"
        );
        assert_eq!(
            sent.header_value("X-Cloud-Api-Client-Signature"),
            Some("sig(quad_cortex-Q0000TEST-with-firmware-4.1.0-at-1700000000-Y2xpZW50)")
        );
        assert_eq!(sent.header_value("X-Cloud-Api-Date"), Some("1700000000"));
        assert_eq!(sent.timeout, Duration::from_millis(40_000));
        assert_eq!(
            sent.body.as_deref(),
            Some(&[][..]),
            "stock posts an empty body"
        );
    }

    /// Stock 0x001f2ccc: the message carries the reply's own timestamp and `activationCode`;
    /// only a valid reply gives the decoded code (0x001f2bf0).
    #[test]
    fn licence_reply_check() {
        let (api, ..) = api();
        let material = crate::cloud::signing::tests::signer().material;
        let message = "quad_cortex-Q0000TEST-with-firmware-4.1.0-at-1700000000-AQID";
        let reply = |signature: &str| json!({ "activationCode": "AQID", "timestamp": 1_700_000_000.0, "signature": signature });
        let good = reply(&material.sign(message));
        let check = |reply: &Value, now_ms| api.verify_licence_reply(&material, reply, now_ms);
        assert_eq!(check(&good, 1_700_000_000_000).unwrap(), Validation::Valid);
        assert_eq!(check(&good, 1_700_000_060_001).unwrap(), Validation::Stale);
        let forged = reply(&material.sign("other"));
        assert_eq!(
            check(&forged, 1_700_000_000_000).unwrap(),
            Validation::Mismatch
        );
        assert!(check(&json!({ "activationCode": "AQID" }), 0).is_err());
        let code = |reply: &Value| api.licence_activation_code(&material, reply, 1_700_000_000_000);
        assert_eq!(code(&good).unwrap().as_bytes(), [1, 2, 3]);
        assert!(code(&forged).is_err());
    }

    #[test]
    fn capture_job_reply() {
        let (mut api, mut http, mut store) = signed_in();
        http.respond(
            200,
            r#"{"id":"j1","status":"pending_recording","progress":12.5,"statusTitle":"Waiting"}"#,
        );
        let job = api.capture_job(&mut http, &mut store, "j1", 0).unwrap();
        assert_eq!(
            http.requests[1].url,
            "https://cloud-api.neuraldsp.com/capture/v1/jobs/j1"
        );
        assert_eq!(
            (job.status.as_str(), job.progress),
            ("pending_recording", Some(12.5))
        );
        assert!(api.capture_job(&mut http, &mut store, "", 0).is_err());
    }
}
