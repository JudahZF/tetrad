//! Directory uploads, device side. A preset (census #3): the record and payload from the library,
//! the grid image, the Neural Capture walk (each user or Downloads capture built for its own
//! upload), then `Services::upload_preset`; after a good reply, the sidecar `cloudId` write. A user
//! capture: the record and payload in the upload cipher, then `Services::upload_capture`; after a
//! good reply, the record `cloudUUID` write. More items of one kind: a batch, one item at a time.

use std::time::Duration;

use tetrad_dsp_host::session::Clock as _;
use tetrad_library::sidecar::Sidecar;
use tetrad_library::stock::{
    CaptureSource, UploadKeys, capture_upload, find_capture, set_capture_cloud_id,
};
use tetrad_preset::{ParamValue, Preset};
use tetrad_services::cloud::products::{CaptureCheck, ProductPayload, parse_date};
use tetrad_services::cloud::services::{ProductKind, ProductUpload};
use tetrad_storage::stock::{DbLimits, ProductType, StockDb};
use tetrad_ui::UploadItem;

use super::control::screen::grid_png;
use super::platform::Platform;
use super::services::{
    BatchItem, CaptureDraft, CaptureOrigin, NO_IR_UPLOAD, PresetDraft, PresetFile, ServiceError,
    WalkCapture, capture_meta_signature,
};
use super::{Device, DeviceError, Effect, Subsystem};

/// Walk end `0x73e4cc` (title "Failed to upload Preset"): a capture is in no store.
const MISSING: &str =
    "This Preset could not be uploaded as one or more Neural Captures are missing.";

/// re-r38 §1 (O): stock has no defined single upload of a Downloads capture (`0x1db0e0` reads
/// the user store only; I: a null record reaches `0x6f9510`).
const DOWNLOADS_SINGLE: &str =
    "Neural Captures in Downloads are uploaded only with a preset that uses them.";

/// The capture models (`0x73cef8`: model − 14000 < 2) and their file parameter.
const CAPTURE_MODELS: [u32; 2] = [14_000, 14_001];
const CAPTURE_FILE: usize = 5;

/// A preset upload refused before the network: the preset name and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRefused {
    pub name: String,
    pub reason: String,
}

impl<P: Platform> Device<P> {
    /// Directory UPLOAD and ⋮ Upload (`0x1db6ac`, Multi Select `0x786060`). One preset is
    /// built here and goes to `Services::upload_preset`, one user capture to
    /// `Services::upload_capture`; more items of one kind are a batch (re-r38 §2,
    /// [`Self::upload_next`]). A refusal before the network ends with the result dialog. IRs
    /// are refused before any request (re-r44 §1: stock 4.1.0 has no IR upload); mixed kinds
    /// are refused (O).
    pub fn upload(
        &mut self,
        items: Vec<UploadItem>,
        now: Duration,
    ) -> Result<Vec<Effect>, DeviceError> {
        self.start_upload(items, false, now)
    }

    /// Cortex Control 46 UPLOAD of one or more keys (`0xf3060`): as [`Self::upload`], but its
    /// questions go to Control (`0x463d2c` flag, re-r38 §3) and a batch is `0x785dcc(..,
    /// fromControl 1)`.
    pub fn control_upload(
        &mut self,
        items: Vec<UploadItem>,
        now: Duration,
    ) -> Result<Vec<Effect>, DeviceError> {
        self.start_upload(items, true, now)
    }

    fn start_upload(
        &mut self,
        items: Vec<UploadItem>,
        from_control: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, DeviceError> {
        let services = &mut self.bridges.services;
        if services.upload_batch_running() {
            return Err(ServiceError::NotReady("a batch upload is in progress").into());
        }
        let keys = |want: fn(&UploadItem) -> Option<&String>| -> Option<Vec<String>> {
            items.iter().map(|item| want(item).cloned()).collect()
        };
        let paths = keys(|item| match item {
            UploadItem::Preset(path) => Some(path),
            _ => None,
        });
        let hashes = keys(|item| match item {
            UploadItem::Capture(hash) => Some(hash),
            _ => None,
        });
        let (kind, keys) = match (paths, hashes) {
            (Some(paths), _) if !paths.is_empty() => (ProductKind::Preset, paths),
            (_, Some(hashes)) if !hashes.is_empty() => (ProductKind::NeuralCapture, hashes),
            _ if items.is_empty() => return Ok(Vec::new()),
            _ if (items.iter()).all(|item| matches!(item, UploadItem::Ir(_))) => {
                return Err(ServiceError::Unsupported(NO_IR_UPLOAD).into());
            }
            _ => {
                return Err(ServiceError::Unsupported(
                    "an upload of presets or captures with other items: one kind at a time",
                )
                .into());
            }
        };
        match (kind, keys.as_slice()) {
            (ProductKind::NeuralCapture, [hash]) => self.upload_capture(hash, from_control, now),
            (_, [path]) => self.upload_preset(path, from_control, now),
            _ => {
                let settings = &mut self.state.settings;
                (services.start_batch(
                    self.platform.services(),
                    settings,
                    kind,
                    keys,
                    from_control,
                    now,
                ))?;
                self.upload_next(now)
            }
        }
    }

    /// The next items of the running batch (`0x847934` captures → `0x1d8278`, `0x848308`
    /// presets → `0x1d81d0`), each as its single upload: an item that ends before the network
    /// lets the next one start at once; else the end of the item in flight calls this again
    /// (the device writes, or `Effect::UploadNext` after an end inside Services). No item
    /// left, or the stop flag: the batch ends (`Services::last_batch_end`).
    pub fn upload_next(&mut self, now: Duration) -> Result<Vec<Effect>, DeviceError> {
        let mut effects = Vec::new();
        while let Some(BatchItem {
            kind,
            key,
            from_control,
        }) = self.bridges.services.batch_next()
        {
            let started = match kind {
                ProductKind::NeuralCapture => self.upload_capture(&key, from_control, now),
                _ => self.upload_preset(&key, from_control, now),
            };
            match started {
                Ok(started) => effects.extend(started),
                // I: an item whose checks fail (no clock, signed out) ends as a failure.
                Err(error) => {
                    let name = match kind {
                        ProductKind::NeuralCapture => self.capture_name(&key),
                        _ => preset_stem(&key).to_owned(),
                    };
                    let log = format!("upload of {name} not started: {error}");
                    effects.push(Effect::Notice(Subsystem::Services, log));
                    (self.bridges.services).end_upload(kind, name, Err(error.to_string()));
                }
            }
            self.bridges.services.batch_item_started();
        }
        Ok(effects)
    }

    /// One preset (`0x1db6ac`): the gate, the draft, then `Services::upload_preset`. The
    /// upload keeps `from_control` (review r39 K3); a refusal changes no other upload.
    fn upload_preset(
        &mut self,
        path: &str,
        from_control: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, DeviceError> {
        let services = &mut self.bridges.services;
        // Review r31 D3: the transfer state, the clock and the account first (`0x1db6ac`), so a
        // refused upload builds nothing.
        let settings = &mut self.state.settings;
        if !(services.upload_gate(self.platform.services(), settings, path, now))? {
            return Ok(Vec::new());
        }
        match self.prepare_upload(path) {
            Ok(draft) => Ok(self.bridges.services.upload_preset(
                self.platform.services(),
                &mut self.state.settings,
                PresetDraft {
                    from_control,
                    ..draft
                },
                now,
            )?),
            Err(UploadRefused { name, reason }) => {
                let log = format!("upload of {name} refused: {reason}");
                let services = &mut self.bridges.services;
                services.end_upload(ProductKind::Preset, name, Err(reason));
                Ok(vec![Effect::Notice(Subsystem::Services, log)])
            }
        }
    }

    /// Directory ⋮ Upload and Control 46 UPLOAD type 2 of one user capture (`0x1db0e0`): the
    /// transfer state and the account first, as for a preset, then the draft and
    /// `Services::upload_capture`. A refusal before the network ends with the result dialog.
    fn upload_capture(
        &mut self,
        hash: &str,
        from_control: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, DeviceError> {
        let services = &mut self.bridges.services;
        let settings = &mut self.state.settings;
        if !(services.capture_upload_gate(self.platform.services(), settings, hash, now))? {
            return Ok(Vec::new());
        }
        match self.prepare_capture_upload(hash) {
            Ok(draft) => Ok(self.bridges.services.upload_capture(
                self.platform.services(),
                &mut self.state.settings,
                CaptureDraft {
                    from_control,
                    ..draft
                },
                now,
            )?),
            Err(UploadRefused { name, reason }) => {
                let log = format!("upload of {name} refused: {reason}");
                let services = &mut self.bridges.services;
                services.end_upload(ProductKind::NeuralCapture, name, Err(reason));
                Ok(vec![Effect::Notice(Subsystem::Services, log)])
            }
        }
    }

    /// The upload of the user capture `hash` (`0x1db0e0` reads the user store only, re-r38
    /// §1): a `product` row of type 2 only (`Self::capture_draft`). A Downloads capture is
    /// refused (O: stock has no defined single upload of one; the preset walk uploads it), as
    /// is a key in no user store. A refusal names the record name when the row reads, else the
    /// key (review r36 H6).
    pub fn prepare_capture_upload(&mut self, hash: &str) -> Result<CaptureDraft, UploadRefused> {
        let found = (self.state.library.as_ref())
            .and_then(|library| library.database.as_ref())
            .map(|db| find_capture(&db.view(), None, hash));
        let reason = match found {
            Some(Ok(Some(CaptureSource::User(_)))) => {
                return self.capture_draft(hash, CaptureOrigin::User);
            }
            Some(Ok(Some(CaptureSource::Download(_)))) => DOWNLOADS_SINGLE.to_owned(),
            Some(Ok(_)) => "the capture is not in the user library".to_owned(),
            Some(Err(error)) => error.to_string(),
            None => "the capture library is not open".to_owned(),
        };
        let name = self.capture_name(hash);
        Err(UploadRefused { name, reason })
    }

    /// The record name of capture `hash` in the user or Downloads store, else the key.
    fn capture_name(&self, hash: &str) -> String {
        (self.capture_record(hash).as_ref())
            .and_then(|fields| fields.text("name"))
            .filter(|name| !name.is_empty())
            .map_or_else(|| hash.to_owned(), str::to_owned)
    }

    /// The record fields of capture `hash` in the user or Downloads store, in the device
    /// cipher as `0x6f6484` reads them; `None` when the row or its metadata does not read.
    fn capture_record(&self, hash: &str) -> Option<Sidecar> {
        let db = (self.state.library.as_ref()).and_then(|library| library.database.as_ref())?;
        let mut metadata = match find_capture(&db.view(), None, hash).ok()?? {
            CaptureSource::User(row) => row.metadata?,
            CaptureSource::Download(row) => row.metadata?,
            CaptureSource::Factory(_) => return None,
        };
        self.media.as_ref()?.cipher().apply(&mut metadata);
        Sidecar::parse(&metadata).ok()
    }

    /// One capture of a preset walk: the check fields from its record (`0x6f94ec` reads the
    /// name, tags and `authorId`) and its own upload ([`Self::capture_draft`]). Review r39 K9:
    /// an upload that does not build is kept as its reason; only a record that does not read
    /// refuses the preset, as the walk cannot check it.
    fn walk_capture(&mut self, hash: &str, origin: CaptureOrigin) -> Result<WalkCapture, String> {
        let fields = (self.capture_record(hash)).ok_or("its record does not read")?;
        let text = |key| fields.text(key).unwrap_or_default().to_owned();
        let check = CaptureCheck {
            hash: hash.to_owned(),
            name: text("name"),
            tags: (fields.texts("tags").into_iter())
                .map(str::to_owned)
                .collect(),
            author_id: text("authorId"),
        };
        let upload = (self.capture_draft(hash, origin))
            .map(|draft| Box::new(draft.upload))
            .map_err(|refused| refused.reason);
        Ok(WalkCapture {
            check,
            origin,
            upload,
        })
    }

    /// The upload of capture `hash` from `origin` (the parts of `0x7bd384`, re-r34 §1; the
    /// record `0x6f6484(key)`). The keys are handles: the device cipher, the seed-only cipher
    /// and the DSP `0x27` key (`tetrad_library::stock::capture_upload`). The body (`0x725020`):
    /// the record fields as its base (`0x7efc38` keeps the record JSON at `+0x88`; I for a
    /// capture) and the stock keys from them; the payload `<hash>.cns` and `<hash>.json`
    /// (`0x6b4c8c`); the meta-signature over the plain metadata (`0x6fffe4`).
    fn capture_draft(
        &mut self,
        hash: &str,
        origin: CaptureOrigin,
    ) -> Result<CaptureDraft, UploadRefused> {
        let name = self.capture_name(hash);
        let refuse = |reason: String| UploadRefused {
            name: name.clone(),
            reason,
        };
        let (global, local) = (self.capture_ciphers())
            .map_err(|error| refuse(format!("the capture keys are not ready: {error}")))?;
        let (request_id, boundary) = (self.upload_ids())
            .ok_or_else(|| refuse("the platform has no random source for the upload id".into()))?;
        let nc2 = (self.media.as_ref()).and_then(|media| media.nc2_cipher().cloned());
        let db = (self.state.library.as_ref())
            .and_then(|library| library.database.as_ref())
            .ok_or_else(|| refuse("the capture library is not open".into()))?;
        let keys = UploadKeys {
            local: &local,
            global: &global,
            nc2: nc2.as_ref(),
        };
        let parts = capture_upload(db, hash, &keys).map_err(|error| refuse(error.to_string()))?;
        let fields = &parts.fields;
        let text = |key| fields.text(key).unwrap_or_default().to_owned();
        let meta_signature = capture_meta_signature(&global, &parts.metadata);
        let payload = ProductPayload::Capture {
            id: hash.to_owned(),
            record: parts.record.clone(),
            metadata: parts.metadata.clone(),
        };
        let payload = payload.pack().map_err(|error| refuse(error.to_string()))?;
        let creator = match text("creator") {
            creator if creator.is_empty() && self.state.platform.is_atma() => "atma".to_owned(),
            creator if creator.is_empty() => "quad".to_owned(),
            creator => creator,
        };
        let upload = ProductUpload {
            kind: ProductKind::NeuralCapture,
            cloud_id: None,
            request_id,
            name: text("name"),
            author_username: text("authorName"),
            author_id: text("authorId"),
            creator,
            creator_version: text("corosVersion"),
            // The record version `+0x40`: "1" when the JSON has none (`0x1ec070`).
            version: fields.text("version").unwrap_or("1").to_owned(),
            date: stock_date(text("date")),
            tags: (fields.texts("tags").into_iter())
                .map(str::to_owned)
                .collect(),
            description: text("description"),
            instrument_type: None,
            dependencies: Vec::new(),
            factory_dependencies: Vec::new(),
            metadata: fields.fields.clone(),
            payload,
            image: None,
            meta_signature,
            boundary,
        };
        Ok(CaptureDraft {
            hash: hash.to_owned(),
            origin,
            upload,
            from_control: false,
        })
    }

    /// A good capture upload reply (`0x1d5944` → `0x7dd13c(record, id)` → user library
    /// `0x82ddfc`: `UPDATE product SET metadata`): the record `cloudUUID` of `hash` is the
    /// product id, in the device cipher, one transaction. Nothing else changes, and nothing
    /// changed before this reply (O). The end goes to the result dialog either way, unless a
    /// preset walk uploaded the capture (`Services::capture_written`).
    pub fn capture_uploaded(
        &mut self,
        hash: &str,
        name: String,
        cloud_id: String,
    ) -> Result<(), DeviceError> {
        let cipher = (self.media.as_ref()).map(|media| media.cipher().clone());
        let db = (self.state.library.as_mut()).and_then(|library| library.database.as_mut());
        let written = match (db, cipher) {
            (Some(db), Some(cipher)) => {
                set_capture_cloud_id(db, hash, &cloud_id, &cipher).map_err(DeviceError::from)
            }
            _ => Err(DeviceError::Unavailable("the capture library")),
        };
        let end = match &written {
            Ok(()) => Ok(cloud_id),
            Err(error) => Err(format!(
                "The Neural Capture was uploaded, but this device could not save its cloud id: \
                 {error}"
            )),
        };
        self.bridges.services.capture_written(hash, name, end);
        let next = self.upload_next(self.clock.now());
        self.apply(next);
        written
    }

    /// The upload of the user preset at `path` (`0x21e000` record, `0x725020` body). The
    /// record is the `.json` sidecar (a preset without one is refused, O). The payload is the
    /// stored preset with the record name and the hash of its path (`0x2e119c`, `0x2e1244`;
    /// `0x681ad4` serializes it), packed as `payload.pb`. The image is the grid of `0x691040`
    /// (a failed render sends none, as a null `QImage` does). The walk (`0x73cef8`,
    /// `0x7414c0`, from the last capture block): a factory capture is a `dependencies.factory`
    /// hash; a user or Downloads capture (`0x6f6484`: user first) is built for its own upload
    /// (`Self::capture_draft`) and Services checks it on the cloud (`0x74102c`; another
    /// author's capture is never uploaded, re-r38 §1); any other capture is missing. The
    /// `.pb` stamp is read before the file (review r37 J4, [`PresetFile`]).
    pub fn prepare_upload(&mut self, path: &str) -> Result<PresetDraft, UploadRefused> {
        let mut name = preset_stem(path).to_owned();
        let refuse = |name: &str, reason: String| UploadRefused {
            name: name.to_owned(),
            reason,
        };
        let (library, specs) = match (&self.state.library, &self.state.specs) {
            (Some(library), Some(specs)) => (library, specs),
            _ => return Err(refuse(&name, "the preset library is not open".into())),
        };
        // Health r34 #3: the factory checks fail closed; only a library without a factory
        // root has no factory presets (review r35 G5: a typed error, not its text).
        let factory = match library.presets.factory_slots() {
            Ok(slots) => slots,
            Err(tetrad_library::Error::NoFactoryRoot) => Vec::new(),
            Err(error) => {
                let reason = format!("the factory preset list does not read: {error}");
                return Err(refuse(&name, reason));
            }
        };
        if factory.iter().flatten().any(|slot| slot == path) {
            return Err(refuse(&name, "a factory preset is not uploaded".into()));
        }
        let host = (library.presets.media().resolve(path))
            .map_err(|error| refuse(&name, error.to_string()))?;
        let file = PresetFile::read(host);
        let mut stored =
            (library.presets.load(path)).map_err(|error| refuse(&name, error.to_string()))?;
        let Some(sidecar) = stored.sidecar.clone() else {
            return Err(refuse(
                &name,
                "the preset has no .json sidecar (its record)".into(),
            ));
        };
        let preset =
            Preset::try_from(&stored.document).map_err(|error| refuse(&name, error.to_string()))?;
        if let Some(named) = (sidecar.text("name").filter(|text| !text.is_empty()))
            .or(preset.name.as_deref().filter(|text| !text.is_empty()))
        {
            name = named.to_owned();
        }
        let factory_db = self.platform.storage().factory.host().join("factory.db");
        let factory_db = StockDb::open_factory(&factory_db, DbLimits::default());
        let (mut factory, mut walk, mut missing) = (Vec::new(), Vec::new(), Vec::new());
        for (key, capture) in capture_pairs(&preset).into_iter().rev() {
            let found = (factory_db.as_ref().map_err(ToString::to_string))
                .and_then(|db| db.view().product(&key).map_err(|error| error.to_string()));
            let in_factory = match found {
                Ok(product) => product.is_some_and(|row| row.product_type == ProductType::Capture),
                Err(error) => {
                    let reason = format!("factory.db does not read: {error}");
                    return Err(refuse(&name, reason));
                }
            };
            let found = (library.database.as_ref())
                .and_then(|db| find_capture(&db.view(), None, &key).ok().flatten());
            match (in_factory, found) {
                (true, _) => factory.push(key),
                (false, Some(CaptureSource::User(_))) => {
                    walk.push((key, capture, CaptureOrigin::User))
                }
                (false, Some(CaptureSource::Download(_))) => {
                    walk.push((key, capture, CaptureOrigin::Downloads));
                }
                _ => missing.push(capture),
            }
        }
        if !missing.is_empty() {
            return Err(refuse(&name, MISSING.into()));
        }
        let image = grid_png(specs, preset).ok();
        let hash = tetrad_core::PresetId::from_identifier(path).0;
        (stored.document)
            .modify(|preset| {
                preset.name = Some(name.clone());
                preset.hash = Some(hash);
                Ok(())
            })
            .map_err(|error| refuse(&name, error.to_string()))?;
        let payload = ProductPayload::Preset(stored.document.to_bytes())
            .pack()
            .map_err(|error| refuse(&name, error.to_string()))?;
        let Some((request_id, boundary)) = self.upload_ids() else {
            return Err(refuse(
                &name,
                "the platform has no random source for the upload id".into(),
            ));
        };
        let mut captures = Vec::with_capacity(walk.len());
        for (key, capture, origin) in walk {
            let walked = (self.walk_capture(&key, origin))
                .map_err(|reason| refuse(&name, format!("Neural Capture {capture}: {reason}")))?;
            captures.push(walked);
        }
        let text = |key| sidecar.text(key).unwrap_or_default().to_owned();
        let date = stock_date(text("date"));
        let creator = match text("creator") {
            creator if creator.is_empty() && self.state.platform.is_atma() => "atma".to_owned(),
            creator if creator.is_empty() => "quad".to_owned(),
            creator => creator,
        };
        let upload = ProductUpload {
            kind: ProductKind::Preset,
            cloud_id: None,
            request_id,
            name: name.clone(),
            author_username: text("author"),
            author_id: text("authorId"),
            creator,
            creator_version: text("corosVersion"),
            // I: record `+0x40`, which the sidecar reader (`0x7efc38`) does not fill.
            version: String::new(),
            date,
            tags: sidecar
                .texts("tags")
                .into_iter()
                .map(str::to_owned)
                .collect(),
            description: text("description"),
            instrument_type: Some(sidecar.instrument().text().to_owned()),
            dependencies: Vec::new(),
            factory_dependencies: factory,
            metadata: sidecar.fields.clone(),
            payload,
            image,
            meta_signature: Vec::new(),
            boundary,
        };
        Ok(PresetDraft {
            path: path.to_owned(),
            file,
            upload,
            captures,
            from_control: false,
        })
    }

    /// The `id` of an upload body (`0x725020`) and its form boundary (`0x80ccf8`): two new
    /// UUIDs (`0x80a778`, `QUuid::createUuid` without braces), version 4, from the platform's
    /// random source.
    fn upload_ids(&mut self) -> Option<(String, String)> {
        let mut uuid = || {
            let mut bytes = [0u8; 16];
            for chunk in bytes.chunks_exact_mut(4) {
                chunk.copy_from_slice(&self.platform.random_u32()?.to_le_bytes());
            }
            bytes[6] = (bytes[6] & 0x0f) | 0x40;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            Some(format!(
                "{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            ))
        };
        Some((uuid()?, uuid()?))
    }

    /// A good upload reply (`0x1d5944` → `0x7dd13c` → `LocalPresetFolder` `0x711aa4`): the
    /// sidecar `cloudId` of the uploaded preset is the product id. O: only that key changes
    /// (stock writes its whole cached record), and nothing else changes before this reply;
    /// Services checked the file stamp just before (review r37 J4). The end goes to the result
    /// dialog either way; a batch goes on with its next item.
    pub fn preset_uploaded(
        &mut self,
        path: &str,
        name: String,
        cloud_id: String,
    ) -> Result<(), DeviceError> {
        let written = self.write_cloud_id(path, &cloud_id);
        let end = match &written {
            Ok(()) => Ok(cloud_id),
            Err(error) => Err(format!(
                "The Preset was uploaded, but this device could not save its cloud id: {error}"
            )),
        };
        let services = &mut self.bridges.services;
        services.end_upload(ProductKind::Preset, name, end);
        let next = self.upload_next(self.clock.now());
        self.apply(next);
        written
    }

    fn write_cloud_id(&mut self, path: &str, cloud_id: &str) -> Result<(), DeviceError> {
        let library =
            (self.state.library.as_mut()).ok_or(DeviceError::Unavailable("the preset library"))?;
        let mut stored = library.presets.load(path)?;
        let sidecar = (stored.sidecar.as_mut())
            .ok_or(DeviceError::Unavailable("the preset's .json sidecar"))?;
        sidecar.set_text("cloudId", cloud_id);
        library.presets.save(&mut stored)?;
        Ok(())
    }
}

/// The file name of a preset path without `.pb`: its name until the record is read.
fn preset_stem(path: &str) -> &str {
    let stem = path.rsplit('/').next().unwrap_or(path);
    stem.strip_suffix(".pb").unwrap_or(stem)
}

/// Review r31 D4: `0x7efc38` reads `date` with the format `yyyy-MM-ddThh:mm:ssZ`
/// (`0x1561d24`) and `0x725020` writes `created` / `updated` with it; any other text is a null
/// `QDateTime`, written as "" (I: the unit's local time is UTC).
fn stock_date(date: String) -> String {
    match date.len() == 20 && parse_date(&date, b'T', "Z").is_some() {
        true => date,
        false => String::new(),
    }
}

/// The capture references of a preset (`0x73cef8`): parameter 5 of each 14000/14001 block at
/// scene 0 (`0x2ea164(block, 0, 5)`), as (`left(64)` the key, `mid(64)` the name), each pair
/// once, in row and block order.
fn capture_pairs(preset: &Preset) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let blocks = preset.chains.iter().flat_map(|chain| &chain.models);
    for block in blocks.filter(|block| {
        block
            .hash
            .is_some_and(|hash| CAPTURE_MODELS.contains(&hash))
    }) {
        let value = match block
            .params
            .get(CAPTURE_FILE)
            .and_then(|param| param.value(0))
        {
            Some(ParamValue::String(value)) => value.as_str(),
            _ => "",
        };
        let key = super::preset::capture_key(value);
        let pair = (key.to_owned(), value[key.len()..].to_owned());
        if !pairs.contains(&pair) {
            pairs.push(pair);
        }
    }
    pairs
}
