//! File (4), receiver `0x250090`, over the stock library: the READ listing, the resync push
//! (`0x25057c`), folder CREATE, the IR import and the preset Save As of `0x24dae0`, preset item
//! edits, and the FileMessageSender pushes of device edits ([`FileEdit`]).
//!
//! A folder is a [`FolderRef`] (cheap: key, flags and where its files come from); only the
//! folders that go out read their files. Keys: a setlist is its directory path, the factory
//! folder [`FACTORY_SETLIST`], a plugin folder its path (I), a media library root
//! `local_ir_root` / `local_nc_root`, a `zenui.db` folder `<id>_q` (user roots included).

use std::collections::BTreeMap;

use tetrad_capture::stored::{device_type_code, instrument_type_code};

use tetrad_control::codecs::{
    ControlMessage, CortexMessageType, EnumValue, FileMessage, FolderInfo, MessageAction,
    ProductData,
};
use tetrad_control::device::{DeviceError as ModelError, DeviceResult};
use tetrad_library::sidecar::Sidecar;
use tetrad_library::stock::{
    CAPTURE_ROOT_KEY, DEFAULT_SETLIST, IR_ROOT_KEY, IrImport, PluginFolder, PluginPreset,
    StockLibrary, create_media_folder, import_user_ir, move_download_capture,
};
use tetrad_media::catalog::{CatalogLimits, FactoryIrCatalog};
use tetrad_media::factory::{DeviceCipher, FACTORY_IR_DIR};
use tetrad_media::ir::IrLimits;
use tetrad_preset::Limits;
use tetrad_preset::model::Preset;
use tetrad_storage::stock::{DownloadKind, FolderType, ProductType, StockDb, StockView};

use super::super::actions::Action;
use super::super::preset;
use super::super::{Effect, FACTORY_SETLIST, State, Subsystem};
use super::{AccountRequest, StateModel, bulk};

mod spread;

use spread::Part;
pub use spread::READS_PER_PASS;
pub(super) use spread::Spread;

const FILE: CortexMessageType = CortexMessageType::File;
/// Owned bounds of an imported WAV (stock reads the whole payload).
const WAV_LIMITS: IrLimits = IrLimits {
    max_asset_bytes: 16 * 1024 * 1024,
    max_chunks: 4096,
};
/// `file_type`: the folder product type (`vt+0x30`).
const PRESETS: i32 = 0;
const IRS: i32 = 1;
const CAPTURES: i32 = 2;
/// The full READ lists presets, captures, then IRs (table `0xa1d588`).
const READ_ORDER: [i32; 3] = [PRESETS, CAPTURES, IRS];

fn refused(what: impl ToString) -> ModelError {
    ModelError::Refused(FILE, what.to_string())
}

fn last_part(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn stem(path: &str) -> &str {
    let file = last_part(path);
    file.strip_suffix(".pb").unwrap_or(file)
}

/// `yyyy-MM-ddThh:mm:ss[Z]` (the sidecar and metadata `date`, UTC) as ms since the epoch.
fn iso_millis(text: &str) -> Option<i64> {
    let (date, time) = text.strip_suffix('Z').unwrap_or(text).split_once('T')?;
    let numbers = |text: &str, separator| -> Option<[i64; 3]> {
        let mut parts = text.splitn(3, separator).map(str::parse::<i64>);
        Some([
            parts.next()?.ok()?,
            parts.next()?.ok()?,
            parts.next()?.ok()?,
        ])
    };
    let [year, month, day] = numbers(date, '-')?;
    let [hour, minute, second] = numbers(time, ':')?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some((((days * 24 + hour) * 60 + minute) * 60 + second) * 1000)
}

/// `0x242ae4` from a sidecar (a preset `.json`, the decrypted metadata of an IR or capture):
/// strings only when not empty, `is_readonly` always. `instrument` (record `+0x5c`, has-bit
/// `0x4000`) and `device` (`+0x60`, `0x8000`) only when not 0 (C): the stock code of the
/// preset `instrument_type` or capture `instrumentType` text (`0x7ec714`) and of the capture
/// `deviceType` text (`0x7ec994`). The record reader `0x7efc38` takes other keys for a capture
/// (type 2, C): `authorName` (`+0x14`), `cloudUUID` (`+0x30`), `gainType` (`+0x2c`, `toInt`
/// default −1; `gain` only when not −1), `version` (`+0x40`, default `"1"`), and no
/// `description`. `shared_by` (has-bit `0x100`) is record `+0x38`, which only the cloud
/// product JSON parser `0x722464` fills (`sharedInfo.sharedBy`, from `0x723188`, `0x72391c`);
/// neither sidecar reader (`0x7ee62c`, `0x7efc38`) reads a `sharedBy` key, so no item listed
/// here has it (C).
fn product(
    key: &str,
    index: Option<i32>,
    sidecar: Option<&Sidecar>,
    fallback: &str,
    read_only: bool,
    capture: bool,
) -> ProductData {
    let text = |name: &str| {
        sidecar
            .and_then(|sidecar| sidecar.text(name))
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let code = |keys: &[&str], map: fn(&str) -> Option<i32>| {
        (keys.iter().find_map(|key| text(key)))
            .as_deref()
            .and_then(map)
            .filter(|&code| code != 0)
    };
    let tags = sidecar.map_or_else(Vec::new, |sidecar| {
        sidecar
            .texts("tags")
            .into_iter()
            .map(str::to_owned)
            .collect()
    });
    // QVariant::toInt: a JSON number or a numeric string.
    let gain = || {
        let value = sidecar?.fields.get("gainType")?;
        let gain = match value.as_str() {
            Some(text) => text.trim().parse().ok()?,
            None => i32::try_from(value.as_i64()?).ok()?,
        };
        (gain != -1).then_some(gain)
    };
    let (author, cloud_id) = match capture {
        true => (text("authorName"), text("cloudUUID")),
        false => (text("author"), text("cloudId")),
    };
    ProductData {
        key: (!key.is_empty()).then(|| key.to_owned()),
        index,
        name: text("name").or_else(|| (!fallback.is_empty()).then(|| fallback.to_owned())),
        author,
        author_id: text("authorId"),
        coros_version: text("corosVersion"),
        tags,
        date_ms_since_epoch: text("date").as_deref().and_then(iso_millis),
        gain: gain().filter(|_| capture),
        cloud_id,
        description: text("description").filter(|_| !capture),
        version: capture.then(|| text("version").unwrap_or_else(|| "1".to_owned())),
        instrument: code(&["instrument_type", "instrumentType"], instrument_type_code),
        device: code(&["deviceType"], device_type_code),
        is_readonly: Some(read_only),
        ..Default::default()
    }
}

/// Where a folder's files come from.
enum Source {
    Setlist(String),
    Factory,
    Plugin(Vec<PluginPreset>),
    /// A media library folder. `full`: the library root ("Captures Library" `lib+0x18`, "IRs
    /// Library" `lib+0x14`), every product of the type with every field; its `id` is the
    /// `zenui.db` user root, where root writes go. Else the `zenui.db` folder `id`, keys only
    /// (the `0x240e68` rule: a sub-folder lists keys).
    Media {
        id: i64,
        full: bool,
    },
    /// A Downloads folder (`0x24592c`): one product per stored row.
    Downloads(DownloadKind),
}

struct FolderRef {
    file_type: i32,
    key: String,
    parent: Option<String>,
    name: String,
    is_factory: bool,
    is_plugin: bool,
    /// The folder flag byte `+8` (`0x7054f0`): a factory plugin folder (I: read-only).
    read_only: bool,
    /// `is_user_default` (`+0x8c`): the default setlist and the `zenui.db` user roots
    /// (`1_q` "My Captures", `2_q` "My IRs"; S7 capture, C).
    user_default: bool,
    source: Source,
}

impl FolderRef {
    /// The tree push skip rule of `0x245800` mode 1 (`omit_factory_content` and resync).
    fn skipped(&self) -> bool {
        self.is_factory || (self.is_plugin && self.read_only)
    }

    /// `0x2416f8`: every scalar is set, false ones too; `parent_key` only with a parent.
    /// `is_cloud` (`+0x64`) and `is_favorites` are false for every folder Tetrad lists (I). A
    /// Downloads folder (`0x24592c`, `0x2415a4`) has only its key, `is_factory` and
    /// `is_downloads` (S7 capture, C).
    fn info(&self, files: Vec<ProductData>) -> FolderInfo {
        if let Source::Downloads(_) = self.source {
            return FolderInfo {
                key: Some(self.key.clone()),
                is_factory: Some(false),
                is_downloads: Some(true),
                files,
                ..Default::default()
            };
        }
        FolderInfo {
            key: Some(self.key.clone()),
            parent_key: self.parent.clone(),
            name: Some(self.name.clone()),
            is_factory: Some(self.is_factory),
            is_downloads: Some(false),
            is_user_default: Some(self.user_default),
            is_cloud: Some(false),
            is_favorites: Some(false),
            is_plugin: Some(self.is_plugin),
            files,
        }
    }

    /// The READ UPDATE of this folder with its built files. Order: Downloads presets by id
    /// (C); Downloads captures by key (stock walks a hash, I); other folders as listed.
    fn finish(&self, mut files: Vec<ProductData>) -> FileMessage {
        if let Source::Downloads(_) = self.source {
            files.sort_by(|a, b| a.key.cmp(&b.key));
        }
        self.push(MessageAction::Update, files)
    }

    fn push(&self, action: MessageAction, files: Vec<ProductData>) -> FileMessage {
        FileMessage {
            action: Some(action.into()),
            file_type: Some(self.file_type),
            folder: Some(self.info(files)),
            ..Default::default()
        }
    }
}

/// One file of a folder before its library read ([`Library::pending`], [`Library::file`]).
enum PendingFile {
    /// Needs no read: an empty slot, a plugin preset, a media key.
    Ready(Box<ProductData>),
    /// A setlist or factory preset: its sidecar.
    Preset {
        path: String,
        index: Option<i32>,
        read_only: bool,
    },
    /// A media library root product: its `zenui.db` row.
    Media { key: String, capture: bool },
    /// A Downloads row: its metadata (a decrypt for a capture), read by `rowid`.
    Download {
        key: String,
        rowid: i64,
        kind: DownloadKind,
    },
}

impl PendingFile {
    /// Whether [`Library::file`] reads the library (a file, a row or a decrypt).
    fn reads(&self) -> bool {
        !matches!(self, Self::Ready(_))
    }
}

/// A plugin preset item: key, name, author, date and `is_readonly` (I: the plugin record fields
/// Tetrad reads from the sidecar).
fn plugin_product(preset: &PluginPreset, read_only: bool) -> ProductData {
    ProductData {
        key: Some(preset.path.clone()),
        name: (!preset.name.is_empty()).then(|| preset.name.clone()),
        author: (!preset.author.is_empty()).then(|| preset.author.clone()),
        date_ms_since_epoch: iso_millis(&preset.date),
        is_readonly: Some(read_only),
        ..Default::default()
    }
}

/// A plugin preset in a writable folder: the `User` folder and below (`0x24812c` counts no
/// read-only folder).
fn writable_plugin_preset(plugin: &PluginFolder, preset: &PluginPreset) -> bool {
    !plugin.read_only || preset.folder == "User" || preset.folder.starts_with("User/")
}

/// The library root key and name of a media type (`0x8297e0` "Captures Library" `0x82b15c`,
/// `0x829440` "IRs Library" `0x8295a0`).
fn media_root(kind: ProductType) -> (&'static str, &'static str) {
    match kind {
        ProductType::Ir => (IR_ROOT_KEY, "IRs Library"),
        _ => (CAPTURE_ROOT_KEY, "Captures Library"),
    }
}

/// The library root folder of `kind` ("Captures Library" `lib+0x18`, "IRs Library"
/// `lib+0x14`): every product of the type; `user_root` is where root writes go.
fn media_root_folder(kind: ProductType, user_root: i64) -> FolderRef {
    let (key, name) = media_root(kind);
    FolderRef {
        file_type: if kind == ProductType::Ir {
            IRS
        } else {
            CAPTURES
        },
        key: key.to_owned(),
        parent: None,
        name: name.to_owned(),
        is_factory: false,
        is_plugin: false,
        read_only: false,
        user_default: false,
        source: Source::Media {
            id: user_root,
            full: true,
        },
    }
}

/// The writable `zenui.db` folder of `kind` that a Control folder key names: the library root
/// key is the user root, `<id>_q` the folder `id` (`0x766a20`, `_q` for `zenui.db`).
fn db_folder(view: &StockView<'_>, kind: ProductType, key: &str) -> DeviceResult<Option<i64>> {
    if key == media_root(kind).0 {
        return view.root_folder(kind).map_err(refused);
    }
    let Some(id) = (key.strip_suffix("_q")).and_then(|id| id.parse::<i64>().ok()) else {
        return Ok(None);
    };
    let folders = view.folders().map_err(refused)?;
    Ok((folders.iter())
        .any(|folder| folder.id == id && folder.product_type == kind && !folder.read_only)
        .then_some(id))
}

/// The library parts a listing reads.
struct Library<'a> {
    presets: &'a StockLibrary,
    database: Option<&'a StockDb>,
    cipher: Option<&'a DeviceCipher>,
}

impl<'a> Library<'a> {
    fn of(model: &'a StateModel<'_>) -> DeviceResult<Self> {
        Self::new(model.state, model.cipher)
    }

    fn new(state: &'a State, cipher: Option<&'a DeviceCipher>) -> DeviceResult<Self> {
        let library = (state.library.as_ref()).ok_or_else(|| refused("no media library"))?;
        Ok(Self {
            presets: &library.presets,
            database: library.database.as_ref(),
            cipher,
        })
    }

    fn database(&self) -> DeviceResult<&'a StockDb> {
        self.database.ok_or_else(|| refused("zenui.db is not open"))
    }

    /// User setlists (`lib+0x08`), each with every field (`0x2453fc(f, 1)`).
    fn setlists(&self) -> DeviceResult<Vec<FolderRef>> {
        let setlists = self.presets.setlists().map_err(refused)?;
        Ok(setlists
            .into_iter()
            .map(|setlist| FolderRef {
                file_type: PRESETS,
                key: setlist.path,
                parent: None,
                user_default: setlist.name == DEFAULT_SETLIST,
                name: setlist.name.clone(),
                is_factory: false,
                is_plugin: false,
                read_only: false,
                source: Source::Setlist(setlist.name),
            })
            .collect())
    }

    /// The factory preset folder (`lib+0x24`), when the factory root is configured.
    fn factory(&self) -> Option<FolderRef> {
        self.presets.factory_slots().ok()?;
        Some(FolderRef {
            file_type: PRESETS,
            key: FACTORY_SETLIST.to_owned(),
            parent: None,
            name: last_part(FACTORY_SETLIST).to_owned(),
            is_factory: true,
            is_plugin: false,
            read_only: true,
            user_default: false,
            source: Source::Factory,
        })
    }

    /// The plugin tree (`lib+0x60`) in pre-order: each plugin, then its folders. A preset's
    /// folder below its plugin (`Neural DSP`, `Artists/<artist>`, `User`) is a sub-folder keyed
    /// `<plugin path>/<folder>` (I); `User` and below are writable. Without `factory_presets`
    /// the read-only folders hold no files and only their direct children are listed: for
    /// listings that skip them ([`FolderRef::skipped`]).
    fn plugins(&self, factory_presets: bool) -> DeviceResult<Vec<FolderRef>> {
        let plugins = match factory_presets {
            true => self.presets.plugin_folders(),
            false => self.presets.writable_plugin_folders(),
        };
        let mut folders = Vec::new();
        for plugin in plugins.map_err(refused)? {
            let mut paths: Vec<String> = plugin.children.clone();
            for preset in &plugin.presets {
                let mut folder = preset.folder.as_str();
                while !folder.is_empty() {
                    if !paths.iter().any(|path| path == folder) {
                        paths.push(folder.to_owned());
                    }
                    folder = folder.rsplit_once('/').map_or("", |(parent, _)| parent);
                }
            }
            // Pre-order: a folder before its children, siblings in name order.
            paths.sort_by_key(|path| path.split('/').map(str::to_lowercase).collect::<Vec<_>>());
            let in_folder = |folder: &str| -> Vec<PluginPreset> {
                (plugin.presets.iter())
                    .filter(|preset| preset.folder == folder)
                    .cloned()
                    .collect()
            };
            folders.push(FolderRef {
                file_type: PRESETS,
                key: plugin.path.clone(),
                parent: None,
                name: plugin.name.clone(),
                is_factory: false,
                is_plugin: true,
                read_only: plugin.read_only,
                user_default: false,
                source: Source::Plugin(in_folder("")),
            });
            for path in &paths {
                let parent = match path.rsplit_once('/') {
                    Some((parent, _)) => format!("{}/{parent}", plugin.path),
                    None => plugin.path.clone(),
                };
                let user = path == "User" || path.starts_with("User/");
                folders.push(FolderRef {
                    file_type: PRESETS,
                    key: format!("{}/{path}", plugin.path),
                    parent: Some(parent),
                    name: last_part(path).to_owned(),
                    is_factory: false,
                    is_plugin: true,
                    read_only: plugin.read_only && !user,
                    user_default: false,
                    source: Source::Plugin(in_folder(path)),
                });
            }
        }
        Ok(folders)
    }

    /// A Downloads folder (`0x7657e0` presets, `0x7657f8` captures; keys `cloud-0-1`,
    /// `cloud-2-1`), when `zenui.db` is open.
    fn downloads(&self, kind: DownloadKind) -> Option<FolderRef> {
        self.database?;
        let folder = (tetrad_library::stock::DOWNLOADS_FOLDERS.iter()).find(|f| f.kind == kind)?;
        Some(FolderRef {
            file_type: folder.file_type,
            key: folder.key.to_owned(),
            parent: None,
            name: String::new(),
            is_factory: false,
            is_plugin: false,
            read_only: false,
            user_default: false,
            source: Source::Downloads(kind),
        })
    }

    /// The media tree of one type in pre-order (`0x245800`, children `0x766dc8`): the library
    /// root, then the `zenui.db` folders in database order, each keyed `<id>_q`; a top folder
    /// (the user root) has the library root as parent (S7 capture: `1_q` "My Captures" below
    /// `local_nc_root` "Captures Library", C). No tree without a user root.
    fn media(&self, kind: ProductType) -> DeviceResult<Vec<FolderRef>> {
        let Some(database) = self.database else {
            return Ok(Vec::new());
        };
        let view = database.view();
        let Some(user_root) = view.root_folder(kind).map_err(refused)? else {
            return Ok(Vec::new());
        };
        let folders: Vec<_> = (view.folders().map_err(refused)?.into_iter())
            .filter(|folder| folder.product_type == kind)
            .collect();
        let file_type = if kind == ProductType::Ir {
            IRS
        } else {
            CAPTURES
        };
        let (root_key, _) = media_root(kind);
        let mut out = vec![media_root_folder(kind, user_root)];
        let mut stack: Vec<i64> = (folders.iter().rev())
            .filter(|folder| folder.parent == 0)
            .map(|folder| folder.id)
            .collect();
        while let Some(id) = stack.pop() {
            let Some(folder) = folders.iter().find(|folder| folder.id == id) else {
                continue;
            };
            out.push(FolderRef {
                file_type,
                key: format!("{id}_q"),
                parent: Some(match folder.parent {
                    0 => root_key.to_owned(),
                    parent => format!("{parent}_q"),
                }),
                name: folder.name.clone(),
                is_factory: false,
                is_plugin: false,
                read_only: folder.read_only,
                user_default: folder.folder_type == FolderType::UserRoot,
                source: Source::Media { id, full: false },
            });
            stack.extend(
                (folders.iter().rev())
                    .filter(|child| child.parent == id && child.id != id)
                    .map(|child| child.id),
            );
        }
        Ok(out)
    }

    /// `0x248ad4(type, omit)`: presets are the setlists, the factory folder unless omitted,
    /// Downloads presets and the plugin tree; captures the capture tree and Downloads
    /// captures; IRs the IR tree. The factory IR folder is no library folder: [`read`] adds
    /// it ([`factory_irs`]).
    fn listing(&self, file_type: i32, omit: bool) -> DeviceResult<Vec<FolderRef>> {
        let tree = |folders: Vec<FolderRef>| {
            let kept = folders
                .into_iter()
                .filter(move |folder| !(omit && folder.skipped()));
            kept.collect::<Vec<_>>()
        };
        Ok(match file_type {
            PRESETS => {
                let mut out = self.setlists()?;
                out.extend(self.factory().filter(|_| !omit));
                out.extend(self.downloads(DownloadKind::Preset));
                out.extend(tree(self.plugins(!omit)?));
                out
            }
            CAPTURES => {
                let mut out = tree(self.media(ProductType::Capture)?);
                out.extend(self.downloads(DownloadKind::Capture));
                out
            }
            IRS => tree(self.media(ProductType::Ir)?),
            _ => Vec::new(),
        })
    }

    fn metadata(&self, blob: &[u8]) -> Option<Sidecar> {
        Sidecar::parse(blob).ok().or_else(|| {
            let mut plain = blob.to_vec();
            self.cipher?.apply(&mut plain);
            Sidecar::parse(&plain).ok()
        })
    }

    /// The full ProductData of one `zenui.db` product.
    fn media_product(&self, id: &str, capture: bool) -> DeviceResult<ProductData> {
        let row = self.database()?.view().product(id).map_err(refused)?;
        let read_only = row.as_ref().is_some_and(|row| row.read_only);
        let sidecar = (row.and_then(|row| row.metadata)).and_then(|blob| self.metadata(&blob));
        Ok(product(id, None, sidecar.as_ref(), "", read_only, capture))
    }

    /// The product ids of a media folder in key order (stock keeps them in a map by id; S7
    /// capture, C): every product of the type for a library root.
    fn media_ids(&self, folder: &FolderRef) -> DeviceResult<Vec<String>> {
        let Source::Media { id, full } = folder.source else {
            return Ok(Vec::new());
        };
        let view = self.database()?.view();
        let kind = if folder.file_type == IRS {
            ProductType::Ir
        } else {
            ProductType::Capture
        };
        let mut ids = match full {
            true => view.product_ids(kind),
            false => view.folder_products(id),
        }
        .map_err(refused)?;
        ids.sort_unstable();
        Ok(ids)
    }

    /// `0x2453fc(f, 1)`: UPDATE with the folder and its files (read-only ones included).
    fn message(&self, folder: &FolderRef) -> DeviceResult<FileMessage> {
        Ok(folder.finish(self.files(self.pending(folder)?)?))
    }

    /// The files of `folder` before their reads, in folder order: one query for a media or
    /// Downloads folder, no sidecar or metadata read. [`Self::file`] builds each file; a
    /// [`Spread`] spends at most [`READS_PER_PASS`] reads per device pass.
    fn pending(&self, folder: &FolderRef) -> DeviceResult<Vec<PendingFile>> {
        let slots = |slots: Vec<Option<String>>, read_only| {
            (slots.into_iter().enumerate())
                .map(|(slot, path)| {
                    let index = i32::try_from(slot).ok();
                    match path {
                        Some(path) => PendingFile::Preset {
                            path,
                            index,
                            read_only,
                        },
                        // An empty slot is `{index, is_readonly}` (S7 capture: 256 items per
                        // setlist, C).
                        None => PendingFile::Ready(Box::new(ProductData {
                            index,
                            is_readonly: Some(read_only),
                            ..Default::default()
                        })),
                    }
                })
                .collect()
        };
        Ok(match &folder.source {
            Source::Setlist(name) => slots(self.presets.slots(name).map_err(refused)?, false),
            Source::Factory => slots(self.presets.factory_slots().map_err(refused)?, true),
            Source::Plugin(presets) => (presets.iter())
                .map(|preset| {
                    PendingFile::Ready(Box::new(plugin_product(preset, folder.read_only)))
                })
                .collect(),
            Source::Media { full, .. } => {
                let capture = folder.file_type == CAPTURES;
                (self.media_ids(folder)?.into_iter())
                    .map(|key| match full {
                        true => PendingFile::Media { key, capture },
                        false => PendingFile::Ready(Box::new(ProductData {
                            key: Some(key),
                            ..Default::default()
                        })),
                    })
                    .collect()
            }
            // One query for the list keys (`0x7ea828`, `0x6a25b0`); the metadata follows in
            // parts ([`Self::files`]). A downloaded capture is keyed by its capture hash, a
            // preset by its cloud id (S7 capture, C).
            Source::Downloads(kind) => (self.database()?.view().download_keys(*kind))
                .map_err(refused)?
                .into_iter()
                .map(|row| PendingFile::Download {
                    key: match (*kind, row.hash) {
                        (DownloadKind::Capture, Some(hash)) => hash,
                        _ => row.id,
                    },
                    rowid: row.rowid,
                    kind: *kind,
                })
                .collect(),
        })
    }

    /// Files of [`Self::pending`] in order ([`Self::file`]); the Downloads metadata of the part
    /// comes from one range read.
    fn files(&self, part: Vec<PendingFile>) -> DeviceResult<Vec<ProductData>> {
        let rows = || {
            part.iter().filter_map(|pending| match pending {
                PendingFile::Download { rowid, kind, .. } => Some((*kind, *rowid)),
                _ => None,
            })
        };
        let mut metadata = BTreeMap::new();
        // One folder holds one kind, in rowid order.
        if let (Some((kind, first)), Some((_, last))) = (rows().next(), rows().next_back()) {
            let view = self.database()?.view();
            metadata.extend(
                view.download_metadata(kind, first..=last)
                    .map_err(refused)?,
            );
        }
        (part.into_iter())
            .map(|pending| match pending {
                PendingFile::Download { key, rowid, kind } => {
                    Ok(self.download_product(&key, kind, metadata.remove(&rowid).flatten()))
                }
                pending => self.file(pending),
            })
            .collect()
    }

    /// A Downloads product from its metadata (a decrypt for a capture).
    fn download_product(
        &self,
        key: &str,
        kind: DownloadKind,
        metadata: Option<Vec<u8>>,
    ) -> ProductData {
        let sidecar = metadata.and_then(|blob| self.metadata(&blob));
        let capture = kind == DownloadKind::Capture;
        product(key, None, sidecar.as_ref(), "", false, capture)
    }

    /// One file of [`Self::pending`]: a setlist preset as `0x242ae4` builds it (its sidecar),
    /// a media library product with every field (its row), a Downloads product (its metadata).
    fn file(&self, pending: PendingFile) -> DeviceResult<ProductData> {
        Ok(match pending {
            PendingFile::Ready(file) => *file,
            PendingFile::Preset {
                path,
                index,
                read_only,
            } => {
                let sidecar = self.presets.sidecar(&path).ok().flatten();
                product(
                    &path,
                    index,
                    sidecar.as_ref(),
                    stem(&path),
                    read_only,
                    false,
                )
            }
            PendingFile::Media { key, capture } => self.media_product(&key, capture)?,
            PendingFile::Download { key, rowid, kind } => {
                let view = self.database()?.view();
                let rows = view.download_metadata(kind, rowid..=rowid);
                let metadata = rows.map_err(refused)?.pop().and_then(|(_, blob)| blob);
                self.download_product(&key, kind, metadata)
            }
        })
    }

    /// `user_content_estimate` (`0x24fb00`): setlist presets (`0x210a74`), writable plugin
    /// presets (`0x24812c`), IR (`0x210a3c`) and capture (`0x2109e4`) library items,
    /// Downloads presets and captures (`vt+0x80` of `0x7657e0`, `0x7657f8`), plus a third of
    /// the media links (the items of the library roots and every sub-folder, `0x705a98`,
    /// minus the root items). The factory capture folders (`lib+0x1c`, `+0x20`) are not built,
    /// so their terms are 0 (I). S7 capture: 86 + 0 + 0 + 42 + 9 + 389 + 42 / 3 = 540, as
    /// stock.
    fn estimate(&self) -> DeviceResult<i32> {
        let mut count: usize = self.stored_items()?.iter().sum();
        for plugin in self.presets.writable_plugin_folders().map_err(refused)? {
            count += (plugin.presets.iter())
                .filter(|preset| writable_plugin_preset(&plugin, preset))
                .count();
        }
        if let Some(database) = self.database {
            let view = database.view();
            let mut links = 0;
            for folder in view.folders().map_err(refused)? {
                if matches!(folder.product_type, ProductType::Ir | ProductType::Capture) {
                    links += view.folder_products(folder.id).map_err(refused)?.len();
                }
            }
            for kind in [DownloadKind::Preset, DownloadKind::Capture] {
                count += view.download_count(kind).map_err(refused)?;
            }
            count += links / 3;
        }
        Ok(i32::try_from(count).unwrap_or(i32::MAX))
    }

    /// Setlist presets (filled slots, `0x210a74`), library captures (`0x2109e4`) and IRs
    /// (`0x210a3c`), in the `StorageInfoType` order.
    fn stored_items(&self) -> DeviceResult<[usize; 3]> {
        let mut presets = 0;
        for setlist in self.presets.setlists().map_err(refused)? {
            let slots = self.presets.slots(&setlist.name).map_err(refused)?;
            presets += slots.iter().flatten().count();
        }
        let products = |kind| match self.database {
            Some(database) => database.view().product_count(kind).map_err(refused),
            None => Ok(0),
        };
        Ok([
            presets,
            products(ProductType::Capture)?,
            products(ProductType::Ir)?,
        ])
    }
}

/// GeneralSettings `storage_info` (36) counts: [`Library::stored_items`].
pub(super) fn stored_items(state: &State) -> DeviceResult<[usize; 3]> {
    Library::new(state, None)?.stored_items()
}

/// `0x25057c`, the resync push: setlists, then the plugin, capture and IR trees with the
/// mode-1 skip rule, then Downloads presets and captures. No estimate or factory folders.
pub(super) fn listing(model: &StateModel<'_>) -> DeviceResult<Vec<FileMessage>> {
    let library = Library::of(model)?;
    let mut folders = library.setlists()?;
    for file_type in [PRESETS, CAPTURES, IRS] {
        let tree = library.listing(file_type, true)?;
        folders.extend(tree.into_iter().filter(|folder| folder.source_is_tree()));
    }
    folders.extend(library.downloads(DownloadKind::Preset));
    folders.extend(library.downloads(DownloadKind::Capture));
    folders
        .iter()
        .map(|folder| library.message(folder))
        .collect()
}

impl FolderRef {
    /// Setlists and the factory folder are not part of the resync trees.
    fn source_is_tree(&self) -> bool {
        matches!(self.source, Source::Plugin(_) | Source::Media { .. })
    }
}

/// The folders whose files fill the transfer state map of a CloudProduct READ
/// ([`Spread::transfer`]): the setlists, the capture library root (every capture), and the
/// Downloads presets and captures.
pub(super) fn synced(model: &StateModel<'_>) -> DeviceResult<Spread> {
    let library = Library::of(model)?;
    let mut folders = library.setlists()?;
    if library.database.is_some() {
        // The capture root lists every capture; its user root is not read here.
        folders.push(media_root_folder(ProductType::Capture, 0));
    }
    folders.extend(library.downloads(DownloadKind::Preset));
    folders.extend(library.downloads(DownloadKind::Capture));
    Ok(Spread::transfer(
        folders,
        model.downloads_in_flight.to_vec(),
        model.upload_states.to_vec(),
    ))
}

/// READ (`0x24fe48`): a folder key lists that folder (any type, read-only files included);
/// a folder without a key lists nothing; `file_type` alone lists that type
/// (`omit_factory_content` applies); neither sends the estimate, then presets, captures and
/// IRs. A type READ of IRs (or of all) without `omit_factory_content` ends with the factory
/// IR folder ([`factory_irs`]). The replies are UPDATE pushes that carry the READ's request
/// ID (`0x2881f0`); the device builds them over several passes ([`Spread`]).
fn read(model: &StateModel<'_>, request: &FileMessage) -> DeviceResult<Spread> {
    let library = Library::of(model)?;
    let omit = request.omit_factory_content.unwrap_or(false);
    let mut parts = Vec::new();
    let folders = match (&request.folder, request.file_type) {
        (Some(folder), _) => match &folder.key {
            Some(key) => library.find(key).ok().into_iter().collect(),
            None => Vec::new(),
        },
        (None, Some(file_type)) => library.listing(file_type, omit)?,
        (None, None) => {
            parts.push(Part::Estimate);
            let mut all = Vec::new();
            for file_type in READ_ORDER {
                all.extend(library.listing(file_type, omit)?);
            }
            all
        }
    };
    parts.extend(folders.into_iter().map(Part::Folder));
    let irs = request.folder.is_none() && matches!(request.file_type, None | Some(IRS));
    if irs
        && !omit
        && let Some(names) = model.factory_irs
    {
        parts.push(Part::Ready(factory_irs(names)));
    }
    Ok(Spread::listing(parts))
}

/// `0x248ad4` type 1, not omitted: after the user IR tree, one UPDATE with `file_type` 1 and
/// a folder keyed [`FACTORY_IR_DIR`] (`is_factory`, `is_downloads` false, no name), one item
/// per name, `name` only. Stock lists `*.bin` in that directory and sends nothing while
/// Control is away ("dropped message").
fn factory_irs(names: &[String]) -> FileMessage {
    let files = (names.iter())
        .map(|name| ProductData {
            name: Some(name.clone()),
            ..Default::default()
        })
        .collect();
    FileMessage {
        action: Some(MessageAction::Update.into()),
        file_type: Some(IRS),
        folder: Some(FolderInfo {
            key: Some(FACTORY_IR_DIR.to_owned()),
            is_factory: Some(true),
            is_downloads: Some(false),
            files,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Owned bounds of the factory IR catalog (the device media library's values).
const CATALOG_LIMITS: CatalogLimits = CatalogLimits {
    max_bytes: 4 << 20,
    max_entities: 4096,
    max_files_per_entity: 64,
};

/// The factory IR item names of `0x248ad4`: each `*.bin` in [`FACTORY_IR_DIR`]
/// (`QDir::entryList`, name order ignoring case), its base name (up to the first `.`) cut at
/// the last `_`, each name once. Tetrad takes the file names from the factory IR catalog,
/// the cache stock rebuilds from that directory (`0x32922c`; I: the cache is current).
pub(super) fn factory_ir_names(catalog: &[u8]) -> Option<Vec<String>> {
    let catalog = FactoryIrCatalog::decode(catalog, CATALOG_LIMITS).ok()?;
    let mut files: Vec<&str> = (catalog.entities().iter())
        .flat_map(|entity| entity.files.iter().map(|file| last_part(&file.path)))
        .collect();
    files.sort_by_cached_key(|file| file.to_lowercase());
    let mut seen = std::collections::HashSet::new();
    let names = files.into_iter().filter_map(|file| {
        let base = file.split('.').next().unwrap_or(file);
        let name = base.rsplit_once('_').map_or(base, |(name, _)| name);
        seen.insert(name).then(|| name.to_owned())
    });
    Some(names.collect())
}

/// `0x24dae0`, the IR import of a File CREATE: the folder named by `folder.key` (`0x76bc68`)
/// must be a writable IR folder (`vt+0x30` == 1, `vt+0x7c`), the payload non-empty and the
/// item name set (`files[0]`, has-bit 2). Stock returns without a word on each failed guard;
/// Tetrad refuses with a notice. The extractor `0x69c384` takes a mono 48 kHz float WAV and
/// its first 1,024 samples; the id is `CIR_` + MD5 (`0x829468`); an existing id is replaced
/// (`vt+0x50`), else added (`vt+0x54`): `tetrad_library::stock::import_user_ir`. The library
/// signal `added(F, key)` pushes CREATE with the folder and the full item (`0x2536ac`). A
/// `zenui.db` write: refused without `--persist`.
fn import_ir(
    model: &mut StateModel<'_>,
    folder: &FolderInfo,
    item: &ProductData,
    wav: &[u8],
) -> DeviceResult<()> {
    model.library_write(FILE)?;
    let key = folder.key.as_deref().unwrap_or_default();
    let name = (item.name.as_deref()).ok_or_else(|| refused("an IR import without a name"))?;
    let date = (model.wall_clock).ok_or_else(|| refused("no wall clock for the import date"))?;
    let cipher = (model.cipher).ok_or_else(|| refused("the device media cipher is not set"))?;
    let database = (model.state.library.as_mut())
        .and_then(|library| library.database.as_mut())
        .ok_or_else(|| refused("zenui.db is not open"))?;
    let target = db_folder(&database.view(), ProductType::Ir, key)?
        .ok_or_else(|| refused(format!("no writable IR folder {key:?}")))?;
    let import = IrImport {
        wav,
        name,
        date,
        cloud_id: item.cloud_id.as_deref().unwrap_or_default(),
        coros_version: tetrad_firmware::running().version,
    };
    let stored = import_user_ir(database, target, &import, cipher, WAV_LIMITS).map_err(refused)?;
    // The id hashes the samples, so a replaced row holds the same samples: no IR cache is
    // stale. The picker and Directory lists are rebuilt.
    model.effects.push(Effect::RefreshDirectory);
    let library = Library::of(model)?;
    let item = library.media_product(&stored.id, false)?;
    let pushed = (library.media(ProductType::Ir)?.into_iter()).find(|folder| folder.key == key);
    if let Some(folder) = pushed {
        let message = folder.push(MessageAction::Create, vec![item]);
        model
            .effects
            .push(Effect::ControlNotify(Box::new(message.into())));
    }
    Ok(())
}

/// CREATE with an item (`0x24e874` → `0x24dae0(item, folder.key, payload, is_plugin)`), on
/// the type of the folder the key names (`0x76bc68`, `vt+0x30`): IRs import `ir_payload`
/// ([`import_ir`]), presets go to [`create_preset`], captures take nothing (`0x24db28`;
/// refused here). `0x24e874` appends `preset_payload` to the payload only when
/// `folder.is_plugin` (`0x24e928`, FolderInfo `+0x31`).
fn create_item(
    model: &mut StateModel<'_>,
    request: &FileMessage,
    folder: &FolderInfo,
    item: &ProductData,
) -> DeviceResult<()> {
    let key = folder.key.as_deref().unwrap_or_default();
    let target = Library::of(model)?.find(key)?;
    match (target.file_type, &request.ir_payload) {
        (IRS, Some(wav)) => import_ir(model, folder, item, wav),
        (IRS, None) => Err(refused("an IR import without a payload")),
        (PRESETS, _) => {
            let payload = (request.preset_payload.as_ref()).map(|raw| raw.bytes.as_slice());
            create_preset(model, folder, target, item, payload)
        }
        _ => Err(ModelError::Unsupported(FILE)),
    }
}

/// The preset branch of `0x24dae0` (`0x24db38`), C:
///
/// - `files[0].index` set (has-bit `0x1000`): Cortex Control Save As. `0x21e42c` takes the
///   record of the target slot; the name is `files[0].name`, else the slot's. Not the loaded
///   slot: `savePresetSignal` (`0x24e0c8`) → `SceneGraph::savePresetSlot` (`0x5b9064` →
///   `0x5b7438`) saves the WORKING preset (`0x510ce0` → `0x1ce40c`, momentary reset
///   `0x51898c`) into the slot, which becomes the loaded one (`0x5b2f40`); then
///   `modifyPresetMetadataSignal` (`0x24e130`). The loaded slot: only that signal, whose slot
///   (`0x5b91dc`) saves the working preset in place. The payload register is never read on
///   this path. In place without `files[0].name` Tetrad keeps the file name (the slot record's
///   name is the sidecar name): a rename by [`Effect::SavePreset`] would change the item key.
///   `instrument` (`0x4000`): [`save_in_place`]; into another slot one save that carries it
///   (stock: the two signals; O, review r47 N1: a failed store then edits nothing).
/// - no index: a plugin preset import, only with `is_plugin` (`0x24db44`): `0x7ed2c8` parses
///   the payload (stock ignores the result) into a record keyed `files[0].key` (FNV-1a id),
///   added or replaced (`vt+0x54`/`vt+0x50`), not loaded: [`import_plugin_preset`].
///
/// Owned guard (O): an indexed CREATE that carries `preset_payload` is refused (stock would
/// save the working preset under the uploaded name, review r19 F1). A save or an import is
/// refused by bring-up without `--persist`.
fn create_preset(
    model: &mut StateModel<'_>,
    folder: &FolderInfo,
    target: FolderRef,
    item: &ProductData,
    payload: Option<&[u8]>,
) -> DeviceResult<()> {
    let Some(index) = item.index else {
        if folder.is_plugin != Some(true) {
            return Err(refused(
                "a preset CREATE without an index outside a plugin folder",
            ));
        }
        return import_plugin_preset(model, target, item, payload);
    };
    if payload.is_some() {
        return Err(refused(
            "a preset payload into a setlist slot (stock saves the working preset instead)",
        ));
    }
    let Source::Setlist(setlist) = &target.source else {
        return Err(ModelError::Unsupported(FILE));
    };
    let slot = usize::try_from(index).map_err(|_| refused(format!("slot {index}")))?;
    let library = Library::of(model)?;
    let slots = library.presets.slots(setlist).map_err(refused)?;
    let held = (slots.get(slot)).ok_or_else(|| refused(format!("slot {index}")))?;
    let name = match (&item.name, held) {
        (Some(name), _) => name.clone(),
        (None, Some(path)) => library.preset_name(path),
        (None, None) => return Err(refused("a Save As into an empty slot without a name")),
    };
    let loaded = held.is_some()
        && (model.state.current.as_ref()).and_then(|current| current.path.as_ref())
            == held.as_ref();
    if loaded {
        return model.queue(FILE, save_in_place(item, name)?);
    }
    // Stock emits the save and the metadata signal (`0x24e0c8`, `0x24e130`); Tetrad makes one
    // save that carries the instrument (O, review r47 N1).
    let save = Effect::SavePresetInto {
        name,
        setlist: Some(target.key),
        slot,
        instrument: preferred_instrument(item)?.unwrap_or_default(),
    };
    model.queue(FILE, save)
}

/// The in-place save of the loaded preset (`modifyPresetMetadataSignal` → `0x5b91dc` →
/// `0x5b7438`): [`Effect::SavePreset`] with the item name; with an item `instrument` Edit
/// Details under the item name, else `name` (the slot's) ([`Effect::EditPresetDetails`]; as
/// on the device, the file then follows the name).
fn save_in_place(item: &ProductData, name: String) -> DeviceResult<Effect> {
    Ok(match preferred_instrument(item)? {
        None => Effect::SavePreset(item.name.clone()),
        Some(instrument) => Effect::EditPresetDetails {
            name: item.name.clone().unwrap_or(name),
            instrument,
        },
    })
}

/// The item `instrument` (has-bit `0x4000` → record `+0x5c`) as a save input: the stock
/// codes 0–5 (`0x7ec4c4`); 0 (not set) is `None`, so the save keeps the stored one. O: another
/// code is refused (stock would write `unknown`).
fn preferred_instrument(
    item: &ProductData,
) -> DeviceResult<Option<tetrad_ui::PreferredInstrument>> {
    use tetrad_library::sidecar::Instrument;
    let Some(code) = item.instrument else {
        return Ok(None);
    };
    let instrument = Instrument::from_code(code)
        .ok_or_else(|| refused(format!("a preset instrument code {code}")))?;
    Ok((instrument != Instrument::NotSet).then(|| preset::preferred_instrument(instrument)))
}

/// The plugin preset import of a CREATE without an index (`0x24db44`, C): the record takes the
/// item's fields (`0x241be4(record, item, 1)`) and the payload (`0x7ed2c8`); a folder that
/// holds `files[0].key` replaces it (`vt+0x50`), else the preset is added (`vt+0x54`):
/// `StockLibrary::store_plugin_preset` (the file is named after the item, the hash is the id of
/// the stored path). The sidecar holds the item's present fields over the new-preset defaults
/// (I: the field rule of `0x241be4` mode 1 is not read); `date` is the item date, else the
/// wall clock; `instrument` is the stock code (`0x7ec4c4`). Owned (O): the payload must pass
/// the typed `BinaryPreset` decode Tetrad loads presets with (stock does not check); a key that
/// names the loaded preset is refused (stock replaces and reloads it, `0x977e54`). Then the
/// library signal pushes `added` (CREATE with the folder and the item) or `changed` (UPDATE
/// with the item and its `old_key`), as for IRs (`0x2536ac`, `0x253d24`; I for plugin
/// folders). The write is refused without `--persist`.
fn import_plugin_preset(
    model: &mut StateModel<'_>,
    target: FolderRef,
    item: &ProductData,
    payload: Option<&[u8]>,
) -> DeviceResult<()> {
    use std::time::{Duration, UNIX_EPOCH};
    use tetrad_library::sidecar::Instrument;
    if !matches!(target.source, Source::Plugin(_)) || target.read_only {
        return Err(refused(
            "a plugin preset import outside a writable plugin folder",
        ));
    }
    let payload = payload.ok_or_else(|| refused("a plugin preset CREATE without a payload"))?;
    Preset::decode(payload, Limits::default())
        .map_err(|error| refused(format!("the uploaded preset does not decode: {error}")))?;
    let document =
        tetrad_preset::PresetDocument::parse(payload, Limits::default()).map_err(refused)?;
    let date = match item.date_ms_since_epoch.map(u64::try_from) {
        Some(Ok(ms)) => UNIX_EPOCH + Duration::from_millis(ms),
        Some(Err(_)) => return Err(refused("a plugin preset date before 1970")),
        None => (model.wall_clock).ok_or_else(|| refused("no wall clock for the preset date"))?,
    };
    let name = (item.name.clone().filter(|name| !name.is_empty()))
        .or_else(|| {
            document
                .summary()
                .ok()?
                .name
                .filter(|name| !name.is_empty())
        })
        .ok_or_else(|| refused("a plugin preset without a name"))?;
    let version = item
        .coros_version
        .as_deref()
        .unwrap_or(tetrad_firmware::running().version);
    let mut sidecar = Sidecar::new_preset(&name, date, version);
    for (field, value) in [
        ("author", &item.author),
        ("authorId", &item.author_id),
        ("cloudId", &item.cloud_id),
        ("description", &item.description),
    ] {
        if let Some(value) = value {
            sidecar.set_text(field, value);
        }
    }
    if !item.tags.is_empty() {
        sidecar
            .fields
            .insert("tags".into(), item.tags.clone().into());
    }
    if let Some(instrument) = item.instrument.and_then(Instrument::from_code) {
        sidecar.set_instrument(instrument);
    }
    let loaded = (model.state.current.as_ref()).and_then(|current| current.path.as_ref());
    if item.key.is_some() && item.key.as_ref() == loaded {
        return Err(refused("a plugin preset import over the loaded preset"));
    }
    model.library_write(FILE)?;
    let library = &mut (model.state.library.as_mut())
        .ok_or_else(|| refused("no media library"))?
        .presets;
    let stored =
        (library.store_plugin_preset(&target.key, item.key.as_deref(), &document, &sidecar))
            .map_err(refused)?;
    if let Some(error) = &stored.tail_error {
        let detail = format!("{}: the old file was not removed: {error}", stored.path);
        model
            .effects
            .push(Effect::Notice(Subsystem::Library, detail));
    }
    model.effects.push(Effect::RefreshDirectory);
    let library = Library::of(model)?;
    let folder = library.find(&target.key)?;
    let Source::Plugin(presets) = &folder.source else {
        return Ok(());
    };
    let product = (presets.iter())
        .find(|preset| preset.path == stored.path)
        .map(|preset| plugin_product(preset, folder.read_only));
    let message = match stored.replaced {
        None => folder.push(MessageAction::Create, product.into_iter().collect()),
        Some(old_key) => {
            let changed = product.map(|item| ProductData {
                old_key: Some(old_key),
                ..item
            });
            folder.push(MessageAction::Update, changed.into_iter().collect())
        }
    };
    model.effects.push(notify(message));
    Ok(())
}

/// CREATE without an item (`0x248968`): `file_type` and `folder.name` are required. IR and
/// capture folders go below `parent_key` (the user root when absent, I) through
/// `create_media_folder` (30 per type, depth below 4); preset folders are setlists (fewer than
/// 12, `0x766764`; `0x76dd8c`). Plugin folders are not built. The library signal
/// `folderCreated` pushes CREATE with the new folder (`0x251f14`). Every type writes the
/// library: refused without `--persist`.
fn create_folder(model: &mut StateModel<'_>, request: &FileMessage) -> DeviceResult<()> {
    let folder = request
        .folder
        .as_ref()
        .ok_or_else(|| refused("CREATE without a folder"))?;
    let (Some(file_type), Some(name)) = (request.file_type, folder.name.as_deref()) else {
        return Ok(());
    };
    if file_type == PRESETS && folder.is_plugin == Some(true) {
        return create_plugin_folder(model, folder.parent_key.as_deref(), name);
    }
    model.library_write(FILE)?;
    let library = (model.state.library.as_mut()).ok_or_else(|| refused("no media library"))?;
    let key = match file_type {
        PRESETS => library.presets.create_setlist(name).map_err(refused)?.path,
        IRS | CAPTURES => {
            let kind = if file_type == IRS {
                ProductType::Ir
            } else {
                ProductType::Capture
            };
            let database = library
                .database
                .as_mut()
                .ok_or_else(|| refused("zenui.db is not open"))?;
            let view = database.view();
            let parent = match folder.parent_key.as_deref().filter(|key| !key.is_empty()) {
                Some(key) => db_folder(&view, kind, key)?,
                None => view.root_folder(kind).map_err(refused)?,
            };
            let parent = parent.ok_or_else(|| refused("no writable parent folder"))?;
            let id = create_media_folder(database, kind, parent, name).map_err(refused)?;
            format!("{id}_q")
        }
        other => return Err(refused(format!("file_type {other}"))),
    };
    model.effects.push(Effect::RefreshDirectory);
    let library = Library::of(model)?;
    let created = (library.listing(file_type, false)?.into_iter()).find(|folder| folder.key == key);
    if let Some(folder) = created {
        let message = folder.push(MessageAction::Create, Vec::new());
        model
            .effects
            .push(Effect::ControlNotify(Box::new(message.into())));
    }
    Ok(())
}

/// The Directory edit of a File UPDATE or DELETE of one folder (no files). UPDATE renames it
/// (`0x24f750` → `0x248548`: `folder.key`, `folder.name`; IR/capture folders other than the
/// root `0x767200`, setlists `0x76a9d0`, plugin folders `0x76aed8`). DELETE removes it: a
/// setlist with its presets (`0x76c170`, I), a `zenui.db` folder with its products when
/// `delete_from_library` (`0x77df68`), a plugin folder with its files (`0x77df68(.., false)`).
/// The device runs the setlist and media edits (`media::edit_directory`); their push follows
/// ([`FileEdit`]). A plugin folder edit is a library write here ([`plugin_folder_edit`]). A
/// media or plugin folder DELETE goes between the 57 pushes of `0x77df68`
/// ([`bulk::folder_removal`]); a setlist DELETE (`0x76c170`) has none. A missing key or name
/// does nothing, as stock returns.
fn folder_edit(model: &mut StateModel<'_>, request: &FileMessage) -> DeviceResult<()> {
    use tetrad_ui::DirectoryRequest as R;
    let Some(info) = &request.folder else {
        return Ok(());
    };
    let Some(key) = &info.key else {
        return Ok(());
    };
    let folder = Library::of(model)?.find(key)?;
    let rename = request.action == Some(MessageAction::Update.into());
    let edit = match (&folder.source, rename) {
        _ if folder.is_factory || folder.read_only => {
            return Err(refused(format!("folder {key:?} is read-only")));
        }
        (Source::Setlist(name), true) => match &info.name {
            Some(to) => R::RenameSetlist {
                from: name.clone(),
                to: to.clone(),
            },
            None => return Ok(()),
        },
        (Source::Setlist(name), false) => R::DeleteSetlist { name: name.clone() },
        (Source::Media { .. }, _) if folder.user_default => {
            return Err(refused("the media root folders cannot change"));
        }
        (Source::Media { full: true, .. }, _) => {
            return Err(refused("the media root folders cannot change"));
        }
        // `0x248548`: not a root, not Downloads.
        (Source::Downloads(_), _) => {
            return Err(refused("the Downloads folders cannot change"));
        }
        (Source::Media { id, .. }, true) => match &info.name {
            Some(name) => R::RenameFolder {
                id: *id,
                name: name.clone(),
            },
            None => return Ok(()),
        },
        (Source::Media { id, .. }, false) => {
            let delete = R::DeleteFolder {
                id: *id,
                remove_products: request.delete_from_library == Some(true),
            };
            let [start, finished] = bulk::folder_removal(folder.info(Vec::new()), folder.file_type);
            model
                .effects
                .extend([start, Effect::Directory(delete), finished]);
            return Ok(());
        }
        (Source::Plugin(_), true) => {
            let name = info.name.clone();
            return plugin_folder_edit(model, &folder, name.as_deref());
        }
        (Source::Plugin(_), false) => return plugin_folder_edit(model, &folder, None),
        (Source::Factory, _) => return Err(ModelError::Unsupported(FILE)),
    };
    model.effects.push(Effect::Directory(edit));
    Ok(())
}

/// Plugin sub-folder CREATE (`0x248968` → `addPluginSubfolder` `0x76f2b8`, C): a sub-folder
/// `name` of the plugin folder `parent` (no parent key: nothing, as `0x76bc68` finds none).
/// The library makes the directory (`StockLibrary::create_plugin_folder`: a writable parent,
/// a plain name, no path that exists, at most `PLUGIN_DEPTH` deep). Then the CREATE push of
/// the new folder (`folderAdded` `0x9e4b30`, I: the same push as a setlist), when the
/// listing shows it (an empty folder two below the plugin is not listed).
fn create_plugin_folder(
    model: &mut StateModel<'_>,
    parent: Option<&str>,
    name: &str,
) -> DeviceResult<()> {
    let Some(parent) = parent.filter(|key| !key.is_empty()) else {
        return Ok(());
    };
    model.library_write(FILE)?;
    let library = (model.state.library.as_mut()).ok_or_else(|| refused("no media library"))?;
    let key = (library.presets)
        .create_plugin_folder(parent, name)
        .map_err(refused)?;
    model.effects.push(Effect::RefreshDirectory);
    let created = (Library::of(model)?.plugins(false)?.into_iter()).find(|f| f.key == key);
    if let Some(folder) = created {
        let message = folder.push(MessageAction::Create, Vec::new());
        model.effects.push(notify(message));
    }
    Ok(())
}

/// A plugin folder UPDATE (`name`: rename, `0x76aed8`) or DELETE (`None`: `0x76ec04`, the
/// folder and every file in it), C. The plugin root and read-only folders stay
/// (`StockLibrary::rename_plugin_folder`, `delete_plugin_folder`). Pushes as the media folder
/// edits (I: `folderRenamed` `0x9e4b6c`, `folderRemoved` `0x9e4adc` reach the same Control
/// slots): a rename UPDATE {folder: the old key, to_folder: the folder}, a delete DELETE
/// {folder}. A favorites write that failed after the change is a notice.
fn plugin_folder_edit(
    model: &mut StateModel<'_>,
    folder: &FolderRef,
    rename: Option<&str>,
) -> DeviceResult<()> {
    model.library_write(FILE)?;
    let message = |action: MessageAction, folder: FolderInfo, to_folder| FileMessage {
        action: Some(action.into()),
        file_type: Some(PRESETS),
        folder: Some(folder),
        to_folder,
        ..Default::default()
    };
    let Some(name) = rename else {
        // `0x77df68(.., false)`: the 57 pushes go around the delete, good or not.
        let [start, finished] = bulk::folder_removal(folder.info(Vec::new()), PRESETS);
        model.effects.push(start);
        let deleted = preset_library(model)
            .and_then(|presets| presets.delete_plugin_folder(&folder.key).map_err(refused));
        let change = match deleted {
            Ok(change) => change,
            Err(error) => {
                model.effects.push(finished);
                return Err(error);
            }
        };
        plugin_folder_changed(model, &change.key, change.tail_error);
        let removed = message(MessageAction::Delete, folder.info(Vec::new()), None);
        model.effects.extend([notify(removed), finished]);
        return Ok(());
    };
    let change = (preset_library(model)?)
        .rename_plugin_folder(&folder.key, name)
        .map_err(refused)?;
    if change.key == folder.key {
        return Ok(());
    }
    plugin_folder_changed(model, &change.key, change.tail_error);
    let renamed = (Library::of(model)?.plugins(false)?.into_iter()).find(|f| f.key == change.key);
    let old = FolderInfo {
        key: Some(folder.key.clone()),
        ..Default::default()
    };
    let push = renamed.map(|to| message(MessageAction::Update, old, Some(to.info(Vec::new()))));
    model.effects.extend(push.map(notify));
    Ok(())
}

fn preset_library<'m>(model: &'m mut StateModel<'_>) -> DeviceResult<&'m mut StockLibrary> {
    (model.state.library.as_mut())
        .map(|library| &mut library.presets)
        .ok_or_else(|| refused("no media library"))
}

/// After a plugin folder rename or delete: a favorites write that failed after the change is a
/// notice, then the Directory refresh.
fn plugin_folder_changed(
    model: &mut StateModel<'_>,
    key: &str,
    tail_error: Option<impl std::fmt::Display>,
) {
    if let Some(error) = tail_error {
        let log = format!("plugin folder {key}: favorites not updated: {error}");
        model.effects.push(Effect::Notice(Subsystem::Control, log));
    }
    model.effects.push(Effect::RefreshDirectory);
}

/// `0x250090`, switched on `action`. READ lists; CREATE with an item imports an IR or saves
/// a preset ([`create_item`]), without one creates a folder; UPDATE and DELETE of a folder
/// without files rename or remove it ([`folder_edit`]), with files edit them ([`item_edit`]);
/// MOVE, COPY and SWAP move or copy a setlist preset ([`slot_move`]). Actions 6 and 7 do
/// nothing. Writes send no reply; their library signals push.
pub(super) fn file(
    model: &mut StateModel<'_>,
    request: &FileMessage,
) -> DeviceResult<Vec<FileMessage>> {
    let action = request.action.unwrap_or(MessageAction::Create.into());
    match action {
        EnumValue::Known(MessageAction::Read) => {
            let spread = read(model, request)?;
            if model.spreads {
                model.spread = Some(spread);
                return Ok(Vec::new());
            }
            let mut replies = Vec::new();
            for message in spread.run(model.state, model.cipher)? {
                match message {
                    ControlMessage::File(file) => replies.push(file),
                    push => model.effects.push(Effect::ControlNotify(Box::new(push))),
                }
            }
            Ok(replies)
        }
        EnumValue::Known(MessageAction::Create) => {
            let folder = request.folder.as_ref();
            let item = folder.and_then(|folder| folder.files.first());
            match (folder, item) {
                (Some(folder), Some(item)) if folder.key.is_some() => {
                    create_item(model, request, folder, item)?
                }
                _ => create_folder(model, request)?,
            }
            Ok(Vec::new())
        }
        EnumValue::Known(MessageAction::Update | MessageAction::Delete)
            if (request.folder.as_ref()).is_some_and(|folder| folder.files.is_empty()) =>
        {
            folder_edit(model, request)?;
            Ok(Vec::new())
        }
        EnumValue::Known(MessageAction::Update | MessageAction::Delete) => {
            let edits = item_edit(model, request)?;
            model.effects.extend(edits);
            Ok(Vec::new())
        }
        EnumValue::Known(
            action @ (MessageAction::Move | MessageAction::Copy | MessageAction::Swap),
        ) => {
            slot_move(model, request, action)?;
            Ok(Vec::new())
        }
        EnumValue::Known(MessageAction::Upload | MessageAction::Download) => Ok(Vec::new()),
        _ => Err(ModelError::Unsupported(FILE)),
    }
}

/// The folder with `key` as `0x76bc68` finds it (any type): `Some((name, slots))` for a user
/// setlist, `None` for any other folder; an unknown key is refused.
pub(super) fn setlist_folder(
    model: &StateModel<'_>,
    key: &str,
) -> DeviceResult<Option<(String, Vec<Option<String>>)>> {
    let library = Library::of(model)?;
    let Source::Setlist(name) = library.find(key)?.source else {
        return Ok(None);
    };
    let slots = library.presets.slots(&name).map_err(refused)?;
    Ok(Some((name, slots)))
}

/// Item UPDATE `0x24eecc` (many `0x781b24`) and DELETE `0x1d916c` (many `0x7835f0`) of IRs
/// and captures in a `zenui.db` folder, one Directory edit per file so the device pushes
/// follow (`changed`, `removed`). UPDATE (`0x241be4`) writes the present fields; Tetrad
/// stores `name`, and for a capture `instrument` and `device` ([`Library::capture_edit`]).
/// Other fields are not stored here (I). DELETE removes the product from the library root, the
/// user root or with `delete_from_library`, else unlinks it from the sub-folder (I; the unlink is a library
/// action, so no `removed` push follows it). Setlist presets: [`preset_item_edit`]. Other
/// preset folders (factory, plugins, Downloads) are not built (`Unsupported`).
fn item_edit(model: &mut StateModel<'_>, request: &FileMessage) -> DeviceResult<Vec<Effect>> {
    use tetrad_ui::DirectoryRequest as R;
    let Some(info) = &request.folder else {
        return Ok(Vec::new());
    };
    let Some(key) = &info.key else {
        return Ok(Vec::new());
    };
    let folder = Library::of(model)?.find(key)?;
    let update = request.action == Some(MessageAction::Update.into());
    let (folder_id, full) = match folder.source {
        Source::Media { id, full } => (id, full),
        Source::Setlist(_) => {
            preset_item_edit(model, folder, &info.files, update)?;
            return Ok(Vec::new());
        }
        _ => return Err(ModelError::Unsupported(FILE)),
    };
    if folder.read_only {
        return Err(refused(format!("folder {key:?} is read-only")));
    }
    let kind = match folder.file_type {
        IRS => tetrad_ui::MediaKind::Ir,
        _ => tetrad_ui::MediaKind::Capture,
    };
    let items = info.files.iter().filter_map(|item| item.key.clone());
    if update {
        let library = Library::of(model)?;
        let mut effects = Vec::new();
        for item in &info.files {
            let Some(id) = item.key.clone() else {
                continue;
            };
            if kind == tetrad_ui::MediaKind::Capture
                && (item.instrument.is_some() || item.device.is_some())
            {
                effects.push(library.capture_edit(id, item)?);
            } else if let Some(name) = item.name.clone() {
                effects.push(Effect::Directory(R::RenameMedia { kind, id, name }));
            }
        }
        return Ok(effects);
    }
    if full || folder.user_default || request.delete_from_library == Some(true) {
        return Ok(items
            .map(|id| Effect::Directory(R::DeleteMedia { kind, id }))
            .collect());
    }
    let products: Vec<String> = items.collect();
    Ok(vec![Effect::Action(
        super::super::actions::Action::LinkMedia {
            folder: folder_id,
            products,
            linked: false,
        },
    )])
}

/// UPDATE (`0x24eecc`) and DELETE (`0x24a570` → `0x7835f0`) of setlist presets, each file by
/// its key (Tetrad: the preset path, I); a key in no slot is skipped (`vt+0x44`). UPDATE writes
/// the present fields into the stored record (`0x241be4(record, item, 0)`); Tetrad stores the
/// name only (I, as for IRs). On the loaded slot `modifyPresetMetadataSignal` follows
/// (`0x24f0a0`), whose slot saves the WORKING preset in place (`0x5b91dc` → `0x5b7438`):
/// [`save_in_place`] (with `instrument`), refused by bring-up without `--persist`; its save
/// pushes. Other slots: [`Action::RenamePreset`], then `changed` (`0x253d24`): UPDATE with the
/// folder and the item with `old_key`; their `instrument` is not stored (no Tetrad input for a
/// preset that is not loaded, O). DELETE: [`Action::DeletePreset`], then `removed`
/// (`0x25270c`): DELETE with the folder and the key.
fn preset_item_edit(
    model: &mut StateModel<'_>,
    folder: FolderRef,
    items: &[ProductData],
    update: bool,
) -> DeviceResult<()> {
    let Source::Setlist(setlist) = &folder.source else {
        return Err(ModelError::Unsupported(FILE));
    };
    for item in items {
        let Some(key) = &item.key else {
            continue;
        };
        let library = Library::of(model)?;
        let slots = library.presets.slots(setlist).map_err(refused)?;
        let Some(slot) = slots.iter().position(|path| path.as_ref() == Some(key)) else {
            continue;
        };
        let loaded = (model.state.current.as_ref())
            .is_some_and(|current| current.path.as_ref() == Some(key));
        if update && loaded {
            let save = save_in_place(item, library.preset_name(key))?;
            model.queue(FILE, save)?;
            continue;
        }
        if update {
            let Some(name) = item
                .name
                .clone()
                .filter(|name| *name != library.preset_name(key))
            else {
                continue;
            };
            let rename = Action::RenamePreset {
                setlist: setlist.clone(),
                slot,
                name,
            };
            model.act(FILE, rename)?;
            let library = Library::of(model)?;
            let changed = library
                .slot_product(&folder, slot)?
                .map(|item| ProductData {
                    old_key: Some(key.clone()),
                    ..item
                });
            let message = folder.push(MessageAction::Update, changed.into_iter().collect());
            model.effects.push(notify(message));
            continue;
        }
        let delete = Action::DeletePreset {
            setlist: setlist.clone(),
            slot,
        };
        model.act(FILE, delete)?;
        let removed = ProductData {
            key: Some(key.clone()),
            ..Default::default()
        };
        model
            .effects
            .push(notify(folder.push(MessageAction::Delete, vec![removed])));
    }
    Ok(())
}

/// MOVE (4) `0x24b804`, COPY (5) `0x24c62c` and SWAP (8) `0x24d2d8` → `0x24b254(key,
/// position, folder.key, to_folder.key, kind)` → `0x1e8a70(…, move = kind 0, force = kind != 2,
/// swap = kind 2)`. Each handler returns without `file_type` (has-bit `0x20`, `0x24b818`,
/// `0x24c640`, `0x24d2ec`); a SWAP also needs [`swap_head`]. Both folders by key, any type
/// (`0x76bc68`; no `to_folder`: the source folder). The item is `folder.files[0].key`; the
/// position is `to_folder.files[0].index` only when `to_folder.files` holds exactly one item
/// with has-bit `0x1000`, else −1 (`0x24b8d0`–`0x24bf28`). By folder (C, `0x1e8a70`): a cloud
/// source (`vt+0x64`) downloads (`0x1dd61c` presets, `0x1d9954` captures), a cloud target
/// uploads (`0x1d81d0`, `0x1d8278`), a plugin source (`vt+0x68`) copies through `0x1e01e8`;
/// else setlist presets ([`preset_move`]) or IRs and captures ([`media_move`]). Tetrad moves
/// setlist presets, setlist ↔ plugin presets ([`setlist_to_plugin`], [`plugin_move`]) and IRs
/// and captures, and uploads into a Downloads folder ([`upload_into_downloads`]: a Downloads
/// folder is a cloud folder), and moves Downloads captures and presets out
/// ([`capture_out_of_downloads`], [`preset_out_of_downloads`]); it lists no cloud folders, and
/// the factory folder and the bulk counter are `Unsupported`. Stock returns on each failed head check;
/// Tetrad refuses with a notice and writes nothing.
fn slot_move(
    model: &mut StateModel<'_>,
    request: &FileMessage,
    action: MessageAction,
) -> DeviceResult<()> {
    let Some(file_type) = request.file_type else {
        return Err(refused("MOVE, COPY or SWAP without file_type"));
    };
    if action == MessageAction::Swap {
        swap_head(request, file_type)?;
    }
    let from = request.folder.as_ref();
    let Some(key) = from.and_then(|folder| folder.key.as_ref()) else {
        return Err(refused("MOVE without a source folder"));
    };
    let to_key = (request.to_folder.as_ref())
        .and_then(|folder| folder.key.as_ref())
        .unwrap_or(key);
    let (source, target) = {
        let library = Library::of(model)?;
        (library.find(key)?, library.find(to_key)?)
    };
    match (&source.source, &target.source) {
        (Source::Setlist(_), Source::Setlist(_)) => {
            preset_move(model, request, action, source, target)
        }
        (Source::Setlist(_), Source::Plugin(_)) => {
            setlist_to_plugin(model, request, action, source, target)
        }
        (Source::Plugin(_), Source::Setlist(_) | Source::Plugin(_)) => {
            let item = (from.and_then(|folder| folder.files.first()))
                .and_then(|item| item.key.as_deref())
                .ok_or_else(|| refused("MOVE without an item key"))?;
            plugin_move(model, request, action, source, target, item)
        }
        // A SWAP is for presets only (`file_type` 0, [`swap_head`]); IR or capture keys in it
        // are refused (O).
        (Source::Media { .. }, Source::Media { .. }) if action == MessageAction::Swap => {
            Err(refused("SWAP of IR or capture items"))
        }
        (Source::Media { .. }, Source::Media { .. }) => {
            let item = (from.and_then(|folder| folder.files.first()))
                .and_then(|item| item.key.as_deref())
                .ok_or_else(|| refused("MOVE without an item key"))?;
            media_move(model, action, source, target, item)
        }
        (Source::Setlist(_) | Source::Media { .. }, Source::Downloads(kind)) => {
            let item = (from.and_then(|folder| folder.files.first()))
                .and_then(|item| item.key.as_deref())
                .ok_or_else(|| refused("MOVE without an item key"))?;
            upload_into_downloads(model, action, &source, *kind, item)
        }
        (Source::Downloads(DownloadKind::Capture), Source::Media { .. }) => {
            let item = (from.and_then(|folder| folder.files.first()))
                .and_then(|item| item.key.as_deref())
                .ok_or_else(|| refused("MOVE without an item key"))?;
            capture_out_of_downloads(model, action, &source, &target, item)
        }
        (Source::Downloads(DownloadKind::Preset), Source::Setlist(_)) => {
            let item = (from.and_then(|folder| folder.files.first()))
                .and_then(|item| item.key.as_deref())
                .ok_or_else(|| refused("MOVE without an item key"))?;
            preset_out_of_downloads(model, request, action, &source, &target, item)
        }
        _ => Err(ModelError::Unsupported(FILE)),
    }
}

/// A MOVE or COPY of a Downloads capture into a user capture folder (C, `0x1d9954` →
/// `0x1d94d4`; MOVE and COPY act the same): the target adds the capture from its holder and
/// the Downloads row goes, in one library write ([`move_download_capture`], which has the
/// stock path). Stock adds with no name check and no question (`0x706f34` → `0x82e91c`), so
/// the store question of a cloud download (`services/store.rs`) is not asked here. Then
/// the UPDATE push of both folders, as [`media_move`]. Refused (O): a SWAP, an IR or
/// read-only target. A Downloads preset: [`preset_out_of_downloads`].
fn capture_out_of_downloads(
    model: &mut StateModel<'_>,
    action: MessageAction,
    source: &FolderRef,
    target: &FolderRef,
    item: &str,
) -> DeviceResult<()> {
    let Source::Media { id: folder, .. } = target.source else {
        return Err(ModelError::Unsupported(FILE));
    };
    if action == MessageAction::Swap {
        return Err(refused("a SWAP out of a Downloads folder"));
    }
    if target.file_type != CAPTURES || target.read_only {
        return Err(refused(
            "a Downloads capture into a folder that takes no captures",
        ));
    }
    model.library_write(FILE)?;
    let db = (model.state.library.as_mut())
        .and_then(|library| library.database.as_mut())
        .ok_or_else(|| refused("no zenui.db"))?;
    move_download_capture(db, item, folder).map_err(refused)?;
    model.effects.push(Effect::RefreshDirectory);
    push_folders(model, [target, source].into_iter())
}

/// A MOVE or COPY of a Downloads preset into a user setlist (C, `0x1e8a70` → `0x1dd61c`; MOVE
/// and COPY act the same): the Downloads folder gives the preset (`vt+0x40`); `0x1dcf2c` asks
/// about an occupied slot (`0x46485c`), else stores it (`0x742990`); after a good store the
/// Downloads row goes (`0x1dc14c`: the source is a Downloads root, `0x7666e8`, so `vt+0x4c`
/// removes the key). The slot is `to_folder.files[0].index` (one item), else the first empty
/// one (I: the −1 path of `0x742990` is not read). Tetrad stores the row's preset and sidecar
/// ([`StockLibrary::store_download_preset`], the cloud download store) and then deletes the
/// row; a failed delete leaves the row (a notice; the preset is stored). Then the UPDATE push of
/// both folders. Refused (O): a SWAP, a read-only target, a full setlist and an occupied slot
/// (stock asks; Tetrad never overwrites from Control here).
fn preset_out_of_downloads(
    model: &mut StateModel<'_>,
    request: &FileMessage,
    action: MessageAction,
    source: &FolderRef,
    target: &FolderRef,
    item: &str,
) -> DeviceResult<()> {
    let Source::Setlist(setlist) = &target.source else {
        return Err(ModelError::Unsupported(FILE));
    };
    if action == MessageAction::Swap {
        return Err(refused("a SWAP out of a Downloads folder"));
    }
    if target.read_only {
        return Err(refused("a Downloads preset into a read-only setlist"));
    }
    let index = match request
        .to_folder
        .as_ref()
        .map(|folder| folder.files.as_slice())
    {
        Some([only]) => only.index.and_then(|index| usize::try_from(index).ok()),
        _ => None,
    };
    model.library_write(FILE)?;
    let library = model
        .state
        .library
        .as_mut()
        .ok_or_else(|| refused("no media library"))?;
    let row = (library.database.as_ref())
        .ok_or_else(|| refused("no zenui.db"))?
        .view()
        .download(DownloadKind::Preset, item)
        .map_err(refused)?
        .ok_or_else(|| refused(format!("no Downloads preset {item:?}")))?;
    let slots = library.presets.slots(setlist).map_err(refused)?;
    let slot = index
        .or_else(|| slots.iter().position(Option::is_none))
        .ok_or_else(|| refused("a Downloads preset into a full setlist"))?;
    if slots.get(slot).is_some_and(Option::is_some) {
        return Err(refused("a Downloads preset onto an occupied slot"));
    }
    let stored = (library.presets)
        .store_download_preset(setlist, slot, &row, None)
        .map_err(refused)?;
    let deleted = (library.database.as_mut())
        .ok_or_else(|| refused("no zenui.db"))?
        .transaction(|write| write.delete_download(DownloadKind::Preset, &row.id));
    let notices = [
        stored.search_index.map(|stale| stale.to_string()),
        (deleted.err()).map(|error| format!("{}: the Downloads row stays: {error}", stored.value)),
    ];
    for detail in notices.into_iter().flatten() {
        model
            .effects
            .push(Effect::Notice(Subsystem::Library, detail));
    }
    model.effects.push(Effect::RefreshDirectory);
    push_folders(model, [target, source].into_iter())
}

/// A MOVE or COPY into a Downloads folder (C): the Downloads folders are cloud folders
/// (vtables `0x122de68` presets, `0x122dc48` captures: `vt+0x64` → 1 `0x716238`, type
/// `vt+0x30` → `+0x3c`, add `vt+0x54` → 2), so `0x24b254` → `0x1e8a70` takes its cloud-target
/// branch: signed out (`0x716b80`) nothing; type 0 → the preset upload `0x1d81d0(source,
/// key)`, type 2 → the capture upload `0x1d8278(key)`, as a CloudProduct (46) UPLOAD of one
/// key. The source keeps the item (MOVE and COPY alike). Tetrad: a user setlist preset into
/// the Downloads presets ([`AccountRequest::CloudProductUpload`]) or a user capture into the
/// Downloads captures ([`AccountRequest::CloudCaptureUpload`]); the item must be in the
/// source folder (`0x24b254`, `vt+0x44`). Refused (O): other pairs (stock would hand a capture
/// key to the preset upload) and a SWAP.
fn upload_into_downloads(
    model: &mut StateModel<'_>,
    action: MessageAction,
    source: &FolderRef,
    kind: DownloadKind,
    item: &str,
) -> DeviceResult<()> {
    if action == MessageAction::Swap {
        return Err(refused("a SWAP into a Downloads folder"));
    }
    if model.state.settings.settings().account.is_logged_out() {
        return Ok(());
    }
    let held = {
        let library = Library::of(model)?;
        match (&source.source, kind) {
            (Source::Setlist(name), DownloadKind::Preset) => {
                let slots = library.presets.slots(name).map_err(refused)?;
                slots.iter().flatten().any(|path| path == item)
            }
            (Source::Media { .. }, DownloadKind::Capture) if source.file_type == CAPTURES => {
                library.media_ids(source)?.iter().any(|key| key == item)
            }
            _ => return Err(refused("an upload into a Downloads folder of another type")),
        }
    };
    if !held {
        return Err(refused(format!(
            "no item {item:?} in folder {:?}",
            source.key
        )));
    }
    let upload = match kind {
        DownloadKind::Preset => AccountRequest::CloudProductUpload {
            path: item.to_owned(),
        },
        DownloadKind::Capture => AccountRequest::CloudCaptureUpload {
            key: item.to_owned(),
        },
    };
    model.queue(FILE, Effect::ControlAccount(upload))
}

/// `0x1d6ec8`: the account as the author of a copied preset, in the document and the sidecar.
fn stamp_account(
    document: &mut tetrad_preset::PresetDocument,
    sidecar: &mut Sidecar,
    account: &preset::SaveAccount,
) -> DeviceResult<()> {
    document
        .modify(|preset| {
            preset.author_name = Some(account.name.clone());
            preset.author_id = Some(account.id.clone());
            Ok(())
        })
        .map_err(refused)?;
    sidecar.set_text("author", &account.name);
    sidecar.set_text("authorId", &account.id);
    Ok(())
}

/// The SWAP head `0x24d2e4`–`0x24d3b0` (C): `file_type` 0 (presets), `to_folder` set,
/// `folder.files` and `to_folder.files` with exactly one item each, the source item with a key
/// (has-bit 1) and the target item with an index (has-bit `0x1000`).
fn swap_head(request: &FileMessage, file_type: i32) -> DeviceResult<()> {
    if file_type != PRESETS {
        return Err(refused(format!("SWAP of file_type {file_type}")));
    }
    fn one(folder: Option<&FolderInfo>) -> Option<&ProductData> {
        match folder?.files.as_slice() {
            [item] => Some(item),
            _ => None,
        }
    }
    let (Some(item), Some(target)) = (
        one(request.folder.as_ref()),
        one(request.to_folder.as_ref()),
    ) else {
        return Err(refused("SWAP without one source and one target item"));
    };
    if item.key.is_none() {
        return Err(refused("SWAP without a source item key"));
    }
    if target.index.is_none() {
        return Err(refused("SWAP without a target slot"));
    }
    Ok(())
}

/// Setlist presets (C): `0x24b254` finds the item by key (`vt+0x44`; for MOVE and COPY Tetrad
/// also takes its `index` when no key is sent, I). The item onto its own slot does nothing
/// (`0x1e8a70`). Else `0x1e8290` with the occupant of the position (`0x70b818`): an empty
/// slot, or a MOVE or COPY (force), drops (`0x1e68d0` → `0x1de9f8`): the preset goes into the
/// slot (no position: the first empty one, `0x7c3d94`) and only a MOVE then removes the
/// source (`0x1de9f8` skips the removal when `move` is clear), so a SWAP onto an empty slot
/// copies. A SWAP always names its target slot ([`swap_head`]). A SWAP onto an occupied slot
/// runs copy-swap `0x1e5a58` at once: the two presets change places, in one setlist or
/// between two (from or onto the factory folder `lib+0x24` it asks on the device screen
/// instead).
///
/// Tetrad: a MOVE inside one setlist and a SWAP onto an occupied slot of the same setlist
/// are [`tetrad_ui::DirectoryRequest::MovePreset`] (an occupied slot swaps; I for the MOVE: the
/// same-setlist branch of `0x1de9f8` is not read). A SWAP onto an occupied slot of another
/// setlist is [`Action::SwapPresets`] (`StockLibrary::swap_between`: each preset becomes a
/// new file in the other setlist, write first), then the swap push `0x2519e8`. Every other
/// case copies ([`Action::CopyPresetTo`]; force: an occupied target is overwritten, no index
/// takes the first empty slot), and a MOVE then deletes the source: the write comes first.
/// The target setlist, and the source of a move, then go out as `0x2453fc(f, 1)` UPDATE (I:
/// the stock sender of this path is not traced). Both actions are refused by bring-up without
/// `--persist`.
fn preset_move(
    model: &mut StateModel<'_>,
    request: &FileMessage,
    action: MessageAction,
    source: FolderRef,
    target: FolderRef,
) -> DeviceResult<()> {
    let (Source::Setlist(from), Source::Setlist(to)) = (&source.source, &target.source) else {
        return Err(ModelError::Unsupported(FILE));
    };
    let index = |info: Option<&FolderInfo>| {
        let item = info?.files.first()?;
        usize::try_from(item.index?).ok()
    };
    let (from_info, to_info) = (request.folder.as_ref(), request.to_folder.as_ref());
    let (from_slots, to_slots) = {
        let library = Library::of(model)?;
        let slots = |setlist: &str| library.presets.slots(setlist).map_err(refused);
        (slots(from)?, slots(to)?)
    };
    let item_key = (from_info.and_then(|f| f.files.first())).and_then(|item| item.key.as_ref());
    let from_slot = match item_key {
        Some(item_key) => (from_slots.iter()).position(|path| path.as_ref() == Some(item_key)),
        None => index(from_info),
    };
    let from_slot =
        from_slot.ok_or_else(|| refused("MOVE of an item that is not in the folder"))?;
    let to_slot = match to_info.map(|folder| folder.files.as_slice()) {
        Some([_]) => index(to_info),
        _ => None,
    };
    let same = from == to;
    if same && to_slot == Some(from_slot) {
        return Ok(());
    }
    let occupied = to_slot.is_some_and(|slot| to_slots.get(slot).is_some_and(Option::is_some));
    let swap = action == MessageAction::Swap;
    if same && (action == MessageAction::Move || (swap && occupied)) {
        let to_slot = to_slot.ok_or_else(|| refused("MOVE without a target slot"))?;
        model
            .effects
            .push(Effect::Directory(tetrad_ui::DirectoryRequest::MovePreset {
                setlist: from.clone(),
                from: from_slot,
                to: to_slot,
                overwrite: false,
            }));
        return Ok(());
    }
    if swap && occupied {
        let to_slot = to_slot.ok_or_else(|| refused("SWAP without a target slot"))?;
        let swap = Action::SwapPresets {
            from: from.clone(),
            slot: from_slot,
            to: to.clone(),
            to_slot,
        };
        model.act(FILE, swap)?;
        // `0x2519e8(record at the source slot, record at the target slot)`, as the swap in one
        // setlist pushes ([`FileEdit`] `Moved`; I: the sender body is not read).
        let library = Library::of(model)?;
        let (moved, other) = (
            library.slot_product(&source, from_slot)?,
            library.slot_product(&target, to_slot)?,
        );
        model.effects.push(notify(FileMessage {
            action: Some(MessageAction::Update.into()),
            file_type: Some(PRESETS),
            folder: Some(source.info(moved.into_iter().collect())),
            to_folder: Some(target.info(other.into_iter().collect())),
            ..Default::default()
        }));
        return Ok(());
    }
    let remove = action == MessageAction::Move;
    let copy = Action::CopyPresetTo {
        from: from.clone(),
        slot: from_slot,
        to: to.clone(),
        to_slot,
        remove,
    };
    model.act(FILE, copy)?;
    let changed = std::iter::once(&target).chain((remove && !same).then_some(&source));
    push_folders(model, changed)
}

/// A setlist preset into a plugin folder (re-r34 §6). Stock (C): `0x24b254` → `0x1e8a70(..,
/// force 1, ctl 1)` → `0x1e8290` (no SWAP or OVERWRITE dialog) → `0x1e68d0` → `0x1de9f8`. No
/// target index: the first empty item of the folder (`0x7c3d94`; I: a plugin folder has none),
/// so nothing is done. Else a MOVE pushes undo frame 3, clears the source slot first (`vt+0x4c`
/// `0x70c47c`), then adds the record (`vt+0x54` → `0x216370`): a read-only folder writes
/// nothing, so the preset is lost; else a free `<name>.pb` (`0x21ee8c`). No author stamp.
///
/// COPY and SWAP (re-r38 §5, C): `0x1e8a70` takes the target item with the source key
/// (`vt+0x44`/`+0x40`) as the occupant; with one nothing happens (I: `0x7658d0` not read), else
/// `0x1e8290` → `0x1de9f8` adds as for a MOVE and keeps the source (no undo frame for a COPY).
/// I: a setlist key is never a plugin folder key, so a SWAP copies too (the SWAP form
/// `0x1e5a58` is not reached; it returns for a plugin target anyway).
///
/// Tetrad (O): the add comes first (`StockLibrary::store_plugin_preset`, a free name); for a
/// MOVE the slot is cleared ([`Action::DeletePreset`]) only after a good add, so a failed add
/// keeps the preset. A read-only folder is refused, and for a MOVE the loaded preset. A COPY
/// pushes no undo frame; the slot delete of a MOVE pushes its delete frame (type 1), whose UNDO
/// puts the setlist slot back and keeps the plugin copy (two copies, nothing lost). Library
/// writes: refused without `--persist`. Then the changed folders go out as UPDATE (I: no reply
/// found).
fn setlist_to_plugin(
    model: &mut StateModel<'_>,
    request: &FileMessage,
    action: MessageAction,
    source: FolderRef,
    target: FolderRef,
) -> DeviceResult<()> {
    let (Source::Setlist(setlist), Source::Plugin(occupants)) = (&source.source, &target.source)
    else {
        return Err(ModelError::Unsupported(FILE));
    };
    let moved = action == MessageAction::Move;
    let indexed = (request.to_folder.as_ref())
        .and_then(|folder| folder.files.first())
        .is_some_and(|item| item.index.is_some_and(|index| index >= 0));
    if !indexed {
        return Ok(());
    }
    if target.read_only {
        return Err(refused("a copy into a read-only plugin folder"));
    }
    let item = (request.folder.as_ref())
        .and_then(|folder| folder.files.first())
        .and_then(|item| item.key.as_deref())
        .ok_or_else(|| refused("MOVE without an item key"))?;
    if occupants.iter().any(|preset| preset.path == item) {
        return Ok(());
    }
    let loaded = (model.state.current.as_ref()).and_then(|current| current.path.as_deref());
    if moved && loaded == Some(item) {
        return Err(refused("a MOVE of the loaded preset"));
    }
    model.library_write(FILE)?;
    let library = &mut (model.state.library.as_mut())
        .ok_or_else(|| refused("no media library"))?
        .presets;
    let slot = (library.slots(setlist).map_err(refused)?.iter())
        .position(|path| path.as_deref() == Some(item))
        .ok_or_else(|| refused(format!("no item {item:?} in folder {:?}", source.key)))?;
    let preset = library.load(item).map_err(refused)?;
    let sidecar = preset.sidecar.unwrap_or_default();
    let stored = (library.store_plugin_preset(&target.key, None, &preset.document, &sidecar))
        .map_err(refused)?;
    if let Some(error) = stored.tail_error {
        let detail = format!("{}: a later step failed: {error}", stored.path);
        model
            .effects
            .push(Effect::Notice(Subsystem::Library, detail));
    }
    model.effects.push(Effect::RefreshDirectory);
    if !moved {
        return push_folders(model, std::iter::once(&target));
    }
    let clear = Action::DeletePreset {
        setlist: setlist.clone(),
        slot,
    };
    // The preset is in the plugin folder now; a failed clear keeps a second copy (O).
    if let Err(error) = model.act(FILE, clear) {
        let detail = format!("{}: the setlist slot stays: {error}", stored.path);
        (model.effects).push(Effect::Notice(Subsystem::Library, detail));
    }
    push_folders(model, [&target, &source].into_iter())
}

/// A plugin preset into a setlist or another plugin folder (`0x1e8a70` checks a plugin source,
/// `vt+0x68`, before any SWAP rule → `0x1e01e8`), C: the item must be in the source folder; the
/// same folder (`vt+0x34`) does nothing.
///
/// - Setlist target (`0x1de8ec` → `0x1de6c4`): MOVE, COPY and SWAP copy, the source stays. The
///   slot is `to_folder.files[0].index` (one item), else the first empty one (I: `0x70e61c`
///   with slot −1 is not read); an occupied slot is overwritten
///   (`StockLibrary::copy_plugin_preset`). Tetrad refuses a copy onto the loaded preset's
///   slot (O, as [`Action::CopyPresetTo`]).
/// - Plugin target (`0x1dfe48`): the preset is added (`StockLibrary::store_plugin_preset`;
///   stock overwrites a preset of the same name, Tetrad takes a free name, O); a MOVE then
///   deletes the source when its folder is writable (`vt+0x4c`). Tetrad refuses a MOVE of the
///   loaded preset (O: the next device save writes its file).
///
/// A copy out of a read-only folder (`0x7054f0`, the factory plugin folders) takes the account as
/// its author before either target (`0x1e01e8` → `0x1d6ec8`, C): the record author and authorId
/// (`+0x14`, `+0x18`) and the preset's own (`0x2df870`, `0x2df8a8`) become the account
/// `ui_username` and token `sub`, as a save does; the tags stay. [`stamp_account`].
///
/// The library writes are refused without `--persist`. Then the changed folders go out as
/// `0x2453fc(f, 1)` UPDATE (`0x1de6c4` and `0x1dfe48` push `0x2515b4` for an overwrite; I for
/// the others).
fn plugin_move(
    model: &mut StateModel<'_>,
    request: &FileMessage,
    action: MessageAction,
    source: FolderRef,
    target: FolderRef,
    item: &str,
) -> DeviceResult<()> {
    let Source::Plugin(presets) = &source.source else {
        return Err(ModelError::Unsupported(FILE));
    };
    if source.key == target.key {
        return Ok(());
    }
    if !presets.iter().any(|preset| preset.path == item) {
        return Err(refused(format!(
            "no item {item:?} in folder {:?}",
            source.key
        )));
    }
    let loaded = (model.state.current.as_ref()).and_then(|current| current.path.clone());
    model.library_write(FILE)?;
    let account =
        (source.read_only).then(|| preset::SaveAccount::of(model.state.settings.settings()));
    let library = &mut (model.state.library.as_mut())
        .ok_or_else(|| refused("no media library"))?
        .presets;
    let remove = match &target.source {
        Source::Setlist(setlist) => {
            let slot = match (request.to_folder.as_ref()).map(|folder| folder.files.as_slice()) {
                Some([one]) => one.index.and_then(|index| usize::try_from(index).ok()),
                _ => None,
            };
            let held = slot.and_then(|slot| library.slots(setlist).ok()?.get(slot)?.clone());
            if held.is_some() && held == loaded {
                return Err(refused("a copy onto the loaded preset's slot"));
            }
            let copied = (library.copy_plugin_preset(item, setlist, slot)).map_err(refused)?;
            if let Some(account) = &account {
                let mut copy = library.load(&copied.value.1).map_err(refused)?;
                let mut sidecar = copy.sidecar.take().unwrap_or_default();
                stamp_account(&mut copy.document, &mut sidecar, account)?;
                copy.sidecar = Some(sidecar);
                library.save(&mut copy).map_err(refused)?;
            }
            // Review r30 C2: the copy is in its slot; a failed drop of the replaced preset is a
            // notice, and the folders are still pushed.
            let tail = (copied.tail_error).map(|error| {
                let path = &copied.value.1;
                format!("copy {path}: the replaced preset was not dropped: {error}")
            });
            let stale = copied.search_index.map(|stale| stale.to_string());
            let notices = stale.into_iter().chain(tail);
            (model.effects)
                .extend(notices.map(|detail| Effect::Notice(Subsystem::Library, detail)));
            false
        }
        Source::Plugin(_) => {
            let remove = action == MessageAction::Move && !source.read_only;
            if remove && loaded.as_deref() == Some(item) {
                return Err(refused("a MOVE of the loaded preset"));
            }
            let mut preset = library.load(item).map_err(refused)?;
            let mut sidecar = preset.sidecar.unwrap_or_default();
            if let Some(account) = &account {
                stamp_account(&mut preset.document, &mut sidecar, account)?;
            }
            let stored =
                (library.store_plugin_preset(&target.key, None, &preset.document, &sidecar))
                    .map_err(refused)?;
            if let Some(error) = stored.tail_error {
                let detail = format!("{}: the old file was not removed: {error}", stored.path);
                model
                    .effects
                    .push(Effect::Notice(Subsystem::Library, detail));
            }
            if remove {
                library.delete_plugin_preset(item).map_err(refused)?;
            }
            remove
        }
        _ => return Err(ModelError::Unsupported(FILE)),
    };
    model.effects.push(Effect::RefreshDirectory);
    let changed = std::iter::once(&target).chain(remove.then_some(&source));
    push_folders(model, changed)
}

/// IRs and captures (C): `0x24b254` needs the item in the source folder (`vt+0x44`; else a
/// log line) and does nothing when the target already holds it. `0x1e8a70` (no setlist
/// target) → `0x1e8290` → the drop `0x1de9f8`: the target adds the item (`vt+0x54`), and only
/// a MOVE removes it from the source (`vt+0x4c`); COPY and SWAP copy. A Downloads captures
/// source is a cloud source (`vt+0x64` → 1): `0x1e8a70` → `0x1d9954`, MOVE and COPY alike
/// (C): a capture the device holds (`0x7db500`, its folder `0x7dbfb8`) is added to the target
/// from that folder's record (`vt+0x40` → target `vt+0x54`), then the Downloads row goes
/// (`0x7666e8` → `vt+0x4c` `0x214c80`); else it downloads again (`0x7061b0`).
///
/// Tetrad: a `zenui.db` folder is a set of links (`folder_content`); the target link goes
/// first, then a MOVE unlinks the source ([`Action::LinkMedia`], write first). Owned: a MOVE
/// out of a root folder keeps the root link (stock calls the root's remove, not read here; a
/// root DELETE deletes the product). The two folders must hold the same product type, the
/// target and a MOVE source must be writable. A Downloads target uploads
/// ([`upload_into_downloads`]); a Downloads captures source moves its row into the user store
/// ([`capture_out_of_downloads`]).
/// Both folders then go out as UPDATE, as for presets (I). Library writes: refused without
/// `--persist`.
fn media_move(
    model: &mut StateModel<'_>,
    action: MessageAction,
    source: FolderRef,
    target: FolderRef,
    item: &str,
) -> DeviceResult<()> {
    let (
        Source::Media {
            id: from,
            full: root,
        },
        Source::Media { id: to, .. },
    ) = (&source.source, &target.source)
    else {
        return Err(ModelError::Unsupported(FILE));
    };
    if source.file_type != target.file_type {
        return Err(refused("a MOVE between IR and capture folders"));
    }
    let (held, already) = {
        let library = Library::of(model)?;
        let holds = |folder: &FolderRef| -> DeviceResult<bool> {
            Ok(library
                .media_ids(folder)?
                .iter()
                .any(|product| product == item))
        };
        (holds(&source)?, holds(&target)?)
    };
    if !held {
        return Err(refused(format!(
            "no item {item:?} in folder {:?}",
            source.key
        )));
    }
    if already {
        return Ok(());
    }
    let unlink = action == MessageAction::Move && !root;
    if target.read_only || (unlink && source.read_only) {
        return Err(refused("a MOVE into or out of a read-only folder"));
    }
    let link = |folder: i64, linked: bool| Action::LinkMedia {
        folder,
        products: vec![item.to_owned()],
        linked,
    };
    model.act(FILE, link(*to, true))?;
    if unlink {
        model.act(FILE, link(*from, false))?;
    }
    let changed = std::iter::once(&target).chain(unlink.then_some(&source));
    push_folders(model, changed)
}

/// `0x2453fc(f, 1)` UPDATE of each changed folder, with its files after the edit. Each folder
/// is listed again by key: a plugin folder's files are a snapshot.
fn push_folders<'f>(
    model: &mut StateModel<'_>,
    folders: impl Iterator<Item = &'f FolderRef>,
) -> DeviceResult<()> {
    let library = Library::of(model)?;
    let pushes = folders
        .map(|folder| library.message(&library.find(&folder.key)?).map(notify))
        .collect::<DeviceResult<Vec<_>>>()?;
    model.effects.extend(pushes);
    Ok(())
}

/// The swap push `0x2519e8` of a library swap undo or redo (`0x649330` case 2 → `0x1e3824`):
/// the same UPDATE as a Control SWAP between setlists ([`preset_move`]), from the slots after
/// the step. Sidecars are read without the device cipher (I: user setlist sidecars are plain).
pub(in crate::device) fn swap_push(
    state: &State,
    sides: &[preset::SwapSide; 2],
) -> DeviceResult<Effect> {
    let library = Library::new(state, None)?;
    let setlists = library.setlists()?;
    let [folder, to_folder] = sides.each_ref().map(|side| {
        let folder = (setlists.iter())
            .find(|folder| matches!(&folder.source, Source::Setlist(name) if *name == side.setlist))
            .ok_or_else(|| refused(format!("no setlist {:?}", side.setlist)))?;
        let item = library.slot_product(folder, side.slot)?;
        Ok(folder.info(item.into_iter().collect()))
    });
    Ok(notify(FileMessage {
        action: Some(MessageAction::Update.into()),
        file_type: Some(PRESETS),
        folder: Some(folder?),
        to_folder: Some(to_folder?),
        ..Default::default()
    }))
}

fn notify(message: FileMessage) -> Effect {
    Effect::ControlNotify(Box::new(message.into()))
}

/// What one device library edit changes, read before the edit runs (a deleted folder or item
/// is gone after it). [`FileEdit::pushes`] builds the FileMessageSender pushes after a
/// successful edit; every push is sent with flags 4 (`0x246c68`) only while Control is
/// connected (`0x6e0444`).
#[derive(Debug, Default)]
pub struct FileEdit(Option<Pending>);

#[derive(Debug)]
enum Pending {
    /// `onFolderRenamed` `0x252c74`: a setlist is found by its new name, a `zenui.db` folder
    /// keeps its key.
    Renamed {
        file_type: i32,
        old_key: String,
        setlist: Option<String>,
    },
    /// `folderRemoved` `0x252310`.
    FolderRemoved { file_type: i32, folder: FolderInfo },
    /// `removed` `0x25270c`, once per folder that held the item.
    ItemRemoved {
        file_type: i32,
        key: String,
        folders: Vec<FolderInfo>,
    },
    /// `changed` `0x253d24`, once per folder that holds the item.
    ItemChanged { file_type: i32, key: String },
    /// `folderCreated` `0x251f14`: the folder keys of the type before the edit.
    Created { file_type: i32, before: Vec<String> },
    /// A setlist slot move: swap `0x2519e8` when both slots hold a preset after it, else the
    /// whole setlist as `0x2453fc(f, 1)` (I).
    Moved {
        setlist: String,
        from: usize,
        to: usize,
    },
}

fn media_type(kind: tetrad_ui::MediaKind) -> (i32, ProductType) {
    match kind {
        tetrad_ui::MediaKind::Ir => (IRS, ProductType::Ir),
        tetrad_ui::MediaKind::Capture => (CAPTURES, ProductType::Capture),
    }
}

impl Library<'_> {
    /// The listed folder with this key, any type. The plugin tree, the costly walk, is read
    /// last.
    fn find(&self, key: &str) -> DeviceResult<FolderRef> {
        let mut folders = self.setlists()?;
        folders.extend(self.factory());
        folders.extend(self.downloads(DownloadKind::Preset));
        folders.extend(self.downloads(DownloadKind::Capture));
        folders.extend(self.media_folders()?);
        if let Some(folder) = folders.into_iter().find(|folder| folder.key == key) {
            return Ok(folder);
        }
        (self.plugins(true)?.into_iter())
            .find(|folder| folder.key == key)
            .ok_or_else(|| refused(format!("no folder {key:?}")))
    }

    /// The `zenui.db` folders (either type) and their file type.
    fn media_folders(&self) -> DeviceResult<Vec<FolderRef>> {
        let mut folders = self.media(ProductType::Ir)?;
        folders.extend(self.media(ProductType::Capture)?);
        Ok(folders)
    }

    /// The media folders of `kind` that hold product `key`, the library root included.
    fn holding(&self, kind: ProductType, key: &str) -> DeviceResult<Vec<FolderRef>> {
        let mut out = Vec::new();
        for folder in self.media(kind)? {
            if self
                .media_ids(&folder)?
                .iter()
                .any(|product| product == key)
            {
                out.push(folder);
            }
        }
        Ok(out)
    }

    fn setlist(&self, key: &str) -> DeviceResult<Option<FolderRef>> {
        Ok(self
            .setlists()?
            .into_iter()
            .find(|folder| folder.key == key))
    }

    /// A capture UPDATE with `instrument` (has-bit `0x4000` → record `+0x5c`) or `device`
    /// (`0x8000` → `+0x60`), both copied as stock codes (`0x241be4`), and `name` (has-bit 1).
    /// An absent field keeps its stored value (an absent name is read from the record). O: a
    /// code outside the stock lists (`device_type_name`, `instrument_type_name`) is refused,
    /// so no edit writes `unknown`.
    fn capture_edit(&self, id: String, item: &ProductData) -> DeviceResult<Effect> {
        let invalid = || {
            refused(format!(
                "capture {id:?} UPDATE with an unknown device or instrument"
            ))
        };
        let device = item.device.map(|code| {
            (u8::try_from(code).ok())
                .and_then(tetrad_ui::CaptureDevice::from_code)
                .ok_or_else(invalid)
        });
        let instrument = item.instrument.map(|code| {
            (u8::try_from(code).ok())
                .and_then(tetrad_ui::CaptureInstrument::from_code)
                .ok_or_else(invalid)
        });
        let name = match &item.name {
            Some(name) => name.clone(),
            None => {
                let database = (self.database).ok_or_else(|| refused("zenui.db is not open"))?;
                let row = database.view().product(&id).map_err(refused)?;
                (row.and_then(|row| row.metadata))
                    .and_then(|blob| self.metadata(&blob))
                    .and_then(|stored| stored.text("name").map(str::to_owned))
                    .ok_or_else(|| refused(format!("capture {id:?} has no stored name")))?
            }
        };
        Ok(Effect::Directory(
            tetrad_ui::DirectoryRequest::EditCapture {
                device: device.transpose()?,
                instrument: instrument.transpose()?,
                id,
                name,
            },
        ))
    }

    /// The name a slot shows for the preset at `path`: its sidecar `name`, else the file stem.
    fn preset_name(&self, path: &str) -> String {
        let sidecar = self.presets.sidecar(path).ok().flatten();
        (sidecar.as_ref().and_then(|sidecar| sidecar.text("name")))
            .filter(|name| !name.is_empty())
            .map_or_else(|| stem(path).to_owned(), str::to_owned)
    }

    /// The preset of one setlist slot as `0x242ae4` builds it.
    fn slot_product(&self, folder: &FolderRef, index: usize) -> DeviceResult<Option<ProductData>> {
        let Source::Setlist(name) = &folder.source else {
            return Ok(None);
        };
        let slots = self.presets.slots(name).map_err(refused)?;
        let Some(path) = slots.into_iter().nth(index).flatten() else {
            return Ok(None);
        };
        let index = i32::try_from(index).ok();
        let preset = PendingFile::Preset {
            path,
            index,
            read_only: false,
        };
        self.file(preset).map(Some)
    }
}

impl FileEdit {
    /// Reads what `request` will change. Requests that write no folder or item (favourites,
    /// recents, loads) push nothing here; their list signals push RecentsFavorites.
    pub fn before(
        request: &tetrad_ui::DirectoryRequest,
        state: &State,
        cipher: Option<&DeviceCipher>,
    ) -> DeviceResult<Self> {
        use tetrad_ui::DirectoryRequest as R;
        let library = Library::new(state, cipher)?;
        let setlist_key = |name: &str| -> DeviceResult<Option<String>> {
            let setlists = library.presets.setlists().map_err(refused)?;
            Ok((setlists.into_iter())
                .find(|setlist| setlist.name == name)
                .map(|setlist| setlist.path))
        };
        let media_folder = |id: i64| -> DeviceResult<Option<FolderRef>> {
            let folders = library.media_folders()?;
            Ok((folders.into_iter()).find(|folder| {
                matches!(folder.source, Source::Media { id: other, full: false } if other == id)
            }))
        };
        let pending = match request {
            R::RenameSetlist { from, to } => setlist_key(from)?.map(|old_key| Pending::Renamed {
                file_type: PRESETS,
                old_key,
                setlist: Some(to.clone()),
            }),
            R::DeleteSetlist { name } => {
                let key = setlist_key(name)?;
                let folder = key.map(|key| library.setlist(&key)).transpose()?.flatten();
                folder.map(|folder| Pending::FolderRemoved {
                    file_type: PRESETS,
                    folder: folder.info(Vec::new()),
                })
            }
            R::MovePreset {
                setlist, from, to, ..
            } => setlist_key(setlist)?.map(|setlist| Pending::Moved {
                setlist,
                from: *from,
                to: *to,
            }),
            R::CreateFolder { kind, .. } => {
                let (file_type, _) = media_type(*kind);
                let before = library.listing(file_type, false)?;
                Some(Pending::Created {
                    file_type,
                    before: before.into_iter().map(|folder| folder.key).collect(),
                })
            }
            R::RenameFolder { id, .. } => media_folder(*id)?.map(|folder| Pending::Renamed {
                file_type: folder.file_type,
                old_key: folder.key,
                setlist: None,
            }),
            R::DeleteFolder { id, .. } => media_folder(*id)?.map(|folder| Pending::FolderRemoved {
                file_type: folder.file_type,
                folder: folder.info(Vec::new()),
            }),
            R::RenameMedia { kind, id, .. } => Some(Pending::ItemChanged {
                file_type: media_type(*kind).0,
                key: id.clone(),
            }),
            R::EditCapture { id, .. } => Some(Pending::ItemChanged {
                file_type: CAPTURES,
                key: id.clone(),
            }),
            R::DeleteMedia { kind, id } => {
                let (file_type, kind) = media_type(*kind);
                let folders = library.holding(kind, id)?;
                Some(Pending::ItemRemoved {
                    file_type,
                    key: id.clone(),
                    folders: folders
                        .iter()
                        .map(|folder| folder.info(Vec::new()))
                        .collect(),
                })
            }
            R::FavoritePreset { .. }
            | R::FavoriteMedia { .. }
            | R::VisitMedia { .. }
            | R::LoadItem { .. }
            | R::Audition { .. } => None,
        };
        Ok(Self(pending))
    }

    /// The pushes after the edit succeeded.
    pub fn pushes(self, state: &State, cipher: Option<&DeviceCipher>) -> DeviceResult<Vec<Effect>> {
        let Some(pending) = self.0 else {
            return Ok(Vec::new());
        };
        let library = Library::new(state, cipher)?;
        let message = |action: MessageAction, file_type: i32| FileMessage {
            action: Some(action.into()),
            file_type: Some(file_type),
            ..Default::default()
        };
        let out = match pending {
            // UPDATE {folder: key only (`0x2415a4`), to_folder: the folder (`0x2416f8`)}.
            Pending::Renamed {
                file_type,
                old_key,
                setlist,
            } => {
                let renamed = match &setlist {
                    Some(name) => (library.setlists()?.into_iter()).find(|f| &f.name == name),
                    None => (library.media_folders()?.into_iter()).find(|f| f.key == old_key),
                };
                renamed
                    .map(|folder| FileMessage {
                        folder: Some(FolderInfo {
                            key: Some(old_key),
                            ..Default::default()
                        }),
                        to_folder: Some(folder.info(Vec::new())),
                        ..message(MessageAction::Update, file_type)
                    })
                    .into_iter()
                    .collect()
            }
            Pending::FolderRemoved { file_type, folder } => vec![FileMessage {
                folder: Some(folder),
                ..message(MessageAction::Delete, file_type)
            }],
            // DELETE {folder + files[{key}]}.
            Pending::ItemRemoved {
                file_type,
                key,
                folders,
            } => (folders.into_iter())
                .map(|folder| FileMessage {
                    folder: Some(FolderInfo {
                        files: vec![ProductData {
                            key: Some(key.clone()),
                            ..Default::default()
                        }],
                        ..folder
                    }),
                    ..message(MessageAction::Delete, file_type)
                })
                .collect(),
            // UPDATE {folder + the full item with `old_key` (has-bit 0x400, I: the key before
            // the change; a `zenui.db` id does not change)}; key-only folders (`0x240e68` = 1)
            // are skipped.
            Pending::ItemChanged { file_type, key } => {
                let kind = if file_type == IRS {
                    ProductType::Ir
                } else {
                    ProductType::Capture
                };
                let item = ProductData {
                    old_key: Some(key.clone()),
                    ..library.media_product(&key, file_type == CAPTURES)?
                };
                (library.holding(kind, &key)?.into_iter())
                    .filter(|folder| matches!(folder.source, Source::Media { full: true, .. }))
                    .map(|folder| FileMessage {
                        folder: Some(folder.info(vec![item.clone()])),
                        ..message(MessageAction::Update, file_type)
                    })
                    .collect()
            }
            // CREATE {folder (no files), file_type}.
            Pending::Created { file_type, before } => (library.listing(file_type, false)?)
                .into_iter()
                .filter(|folder| !before.contains(&folder.key))
                .map(|folder| folder.push(MessageAction::Create, Vec::new()))
                .collect(),
            Pending::Moved { setlist, from, to } => {
                let Some(folder) = library.setlist(&setlist)? else {
                    return Ok(Vec::new());
                };
                match (
                    library.slot_product(&folder, to)?,
                    library.slot_product(&folder, from)?,
                ) {
                    (Some(moved), Some(other)) => vec![FileMessage {
                        folder: Some(folder.info(vec![moved])),
                        to_folder: Some(folder.info(vec![other])),
                        ..message(MessageAction::Update, PRESETS)
                    }],
                    _ => vec![library.message(&folder)?],
                }
            }
        };
        Ok(out.into_iter().map(notify).collect())
    }
}

/// The Downloads push `0x250634(control, type, id)` after a store (`0x71d538`): CREATE,
/// `file_type` of the folder (has-bit `0x20`), the Downloads folder (`0x2416f8`, has-bit 2)
/// holding the stored product (`0x242ae4`, `key` = `id`). The action word `+0x28` is 0 =
/// CREATE (C). Nothing when the row is not found (`vt+0x40`).
pub(in crate::device) fn download_stored(
    state: &State,
    cipher: Option<&DeviceCipher>,
    kind: DownloadKind,
    id: &str,
) -> DeviceResult<Vec<Effect>> {
    let library = Library::new(state, cipher)?;
    let (Some(folder), Some(database)) = (library.downloads(kind), library.database) else {
        return Ok(Vec::new());
    };
    let Some(row) = database.view().download(kind, id).map_err(refused)? else {
        return Ok(Vec::new());
    };
    let sidecar = (row.metadata).and_then(|blob| library.metadata(&blob));
    let capture = kind == DownloadKind::Capture;
    let item = product(id, None, sidecar.as_ref(), "", false, capture);
    Ok(vec![notify(folder.push(MessageAction::Create, vec![item]))])
}

/// The listed folder `key` (any type) as `0x2416f8` builds it, with no files: the
/// BulkOperation (57) source folder. `None` when no listed folder has the key.
pub(super) fn folder_info(
    state: &State,
    cipher: Option<&DeviceCipher>,
    key: &str,
) -> Option<FolderInfo> {
    let library = Library::new(state, cipher).ok()?;
    Some(library.find(key).ok()?.info(Vec::new()))
}

/// The user setlist named `name` as `0x2416f8` builds it, with no files: the BulkOperation
/// (57) target of a preset download run. `None` when no setlist has the name.
pub(super) fn setlist_info(
    state: &State,
    cipher: Option<&DeviceCipher>,
    name: &str,
) -> Option<FolderInfo> {
    let library = Library::new(state, cipher).ok()?;
    let setlists = library.setlists().ok()?;
    let folder = setlists.into_iter().find(|folder| folder.name == name)?;
    Some(folder.info(Vec::new()))
}

/// The saving destination of `kind` (`0x76be70(root, type)`: the stored
/// `downloadDestinationFolderNC` / `IR`, else the user root) as `0x2416f8` builds it, with no
/// files: the BulkOperation (57) target of a capture or IR download run. `None` with no
/// library or no such folder.
pub(super) fn saving_destination_info(
    state: &State,
    cipher: Option<&DeviceCipher>,
    kind: ProductType,
) -> Option<FolderInfo> {
    let library = Library::new(state, cipher).ok()?;
    let view = library.database?.view();
    let settings = &state.settings.settings().library;
    let stored = match kind {
        ProductType::Ir => &settings.download_destination_ir,
        _ => &settings.download_destination_nc,
    };
    let id = tetrad_library::stock::saving_destination(&view, kind, stored.as_deref()).ok()?;
    Some(library.find(&format!("{id}_q")).ok()?.info(Vec::new()))
}

/// The preset save push (`0x2515b4`, `savePresetSignal` path): UPDATE, `file_type` 0, the
/// setlist folder with the saved preset. Nothing when the saved preset is in no user setlist.
pub(in crate::device) fn preset_saved(
    state: &State,
    cipher: Option<&DeviceCipher>,
) -> DeviceResult<Vec<Effect>> {
    let library = Library::new(state, cipher)?;
    let Some(path) = state
        .current
        .as_ref()
        .and_then(|current| current.path.as_deref())
    else {
        return Ok(Vec::new());
    };
    let slot = super::super::preset::locate(library.presets, path).map_err(refused)?;
    let Some(super::super::PresetRef {
        setlist: Some(key),
        index,
    }) = slot
    else {
        return Ok(Vec::new());
    };
    let Some(folder) = library.setlist(&key)? else {
        return Ok(Vec::new());
    };
    let item = library.slot_product(&folder, index as usize)?;
    Ok(item
        .map(|item| {
            notify(FileMessage {
                action: Some(MessageAction::Update.into()),
                file_type: Some(PRESETS),
                folder: Some(folder.info(vec![item])),
                ..Default::default()
            })
        })
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use tetrad_library::sidecar::Sidecar;

    /// A13: `shared_by` comes only from cloud product JSON (`0x722464`), never from a sidecar
    /// or Downloads metadata, so a listed item has none even when its sidecar names one.
    #[test]
    fn a_listed_item_never_carries_shared_by() {
        let json = br#"{"name":"Shared","sharedBy":"friend","sharedInfo":{"sharedBy":"friend"}}"#;
        let sidecar = Sidecar::parse(json).unwrap();
        for capture in [false, true] {
            let item = super::product("key", None, Some(&sidecar), "", false, capture);
            assert_eq!(item.name.as_deref(), Some("Shared"));
            assert_eq!(item.shared_by, None);
        }
    }
}
