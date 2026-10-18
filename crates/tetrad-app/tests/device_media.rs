//! The IR library (`device::media`, w2b-dsp M4): a cab `ir selector` resolves to its factory
//! entity's files, decrypted, in catalog order and read once per process; unknown values stay
//! absent so the driver loads the stock default IR. All bytes are invented.

mod common;

use std::time::Duration;

use tetrad_app::device::media::MediaLibrary;
use tetrad_app::device::platform::DspFile;
use tetrad_dsp_host::setup::drivers::media::CabIrs;
use tetrad_media::catalog::{FactoryIrCatalog, FactoryIrEntity, FactoryIrFile, MicPlacement};
use tetrad_media::factory::{DeviceCipher, FACTORY_IR_DIR};

fn cipher() -> DeviceCipher {
    DeviceCipher::caller_secret(b"invented test secret")
}

/// One encrypted 4,096-byte factory image: an impulse of `level`.
fn image(level: f32) -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..1024)
        .flat_map(|index| if index == 0 { level } else { 0.0f32 }.to_le_bytes())
        .collect();
    cipher().apply(&mut bytes);
    bytes
}

#[test]
fn factory_selector_loads_its_files_once_and_unknown_values_stay_absent() {
    let files = ["A0", "B1"].map(|suffix| FactoryIrFile {
        path: format!("{FACTORY_IR_DIR}/T1_Cab_Mic_{suffix}.bin"),
        placement: MicPlacement::parse(suffix).unwrap(),
    });
    let catalog = FactoryIrCatalog::new(vec![FactoryIrEntity {
        tag: "T1".into(),
        cab: "Cab".into(),
        mic: "Mic".into(),
        readable_name: "T1 Cab Mic".into(),
        files: Vec::from(files),
    }])
    .encode();
    let mut library = MediaLibrary::new(&catalog, cipher()).unwrap();
    let mut reads = Vec::new();
    let mut read = |file: DspFile<'_>| match file {
        DspFile::FactoryIr(path) => {
            reads.push(path.to_owned());
            Ok(image(0.5))
        }
        other => Err(format!("unexpected {other:?}")),
    };
    let values = ["T1_Cab_Mic", "T9_Gone_Mic", ""];
    let (media, failures) = library.resolve(values, &mut read, None);
    assert!(failures.is_empty(), "{failures:?}");
    match media.cabs.get("T1_Cab_Mic") {
        Some(CabIrs::Factory { tag, irs }) => assert_eq!((tag.as_str(), irs.len()), ("T1", 2)),
        other => panic!("expected the factory entity, got {other:?}"),
    }
    assert!(!media.cabs.contains_key("T9_Gone_Mic"));
    // Cached for the process: the next push reads no file again.
    let (again, _) = library.resolve(values, &mut read, None);
    assert!(again.cabs.contains_key("T1_Cab_Mic"));
    assert_eq!(
        reads,
        [
            format!("{FACTORY_IR_DIR}/T1_Cab_Mic_A0.bin"),
            format!("{FACTORY_IR_DIR}/T1_Cab_Mic_B1.bin"),
        ]
    );
}

const DIRECTORY_DDL: &str = "
CREATE TABLE default_param (model_id TEXT,param_id INT,value TEXT,exp INT,exp_trim_max REAL,exp_trim_min REAL,PRIMARY KEY (model_id, param_id));
CREATE TABLE default_exp_switch (model_id TEXT,exp_switch_type INT,exp_switch_inverted INT,exp_switch_latch_emulation INT,exp_switch_delay_ms INT,PRIMARY KEY (model_id));
CREATE TABLE product ( id TEXT, type INT, readonly INT, rawdata BLOB, metadata BLOB, PRIMARY KEY(id));
CREATE TABLE downloads_product_preset ( id TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY ( id ) );
CREATE TABLE downloads_product_nc ( id TEXT, hash TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY ( id ) );
CREATE INDEX downloads_product_nc_hash ON downloads_product_nc (hash);
CREATE TABLE folder ( id INTEGER PRIMARY KEY AUTOINCREMENT, pdType INT, folderType INT, name TEXT, parent INTEGER, readonly INT);
CREATE TABLE folder_content ( folderId INTEGER, productId TEXT, FOREIGN KEY(folderId) REFERENCES folder(id), PRIMARY KEY(folderId,productId));
CREATE TABLE nc_encryption ( nc_type TEXT, encryption INT, PRIMARY KEY ( nc_type ) );
CREATE TABLE pinned_models (hash TEXT);
CREATE TABLE model_presets ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, hash INT, expSwitchType INT, expSwitchInverted INT, expSwitchLatchEmulation INT, expSwitchDelayMs INT);
CREATE TABLE default_model_presets ( hash INT PRIMARY KEY, presetId INTEGER, isFactory INTEGER );
CREATE TABLE model_preset_parameters (presetId INT, paramIndex INT, value TEXT, exp INT, expTrimMax REAL, expTrimMin REAL, FOREIGN KEY (presetId) REFERENCES model_presets(id), PRIMARY KEY (presetId, paramIndex));
CREATE TABLE schema_version ( major INTEGER, minor INTEGER);
INSERT INTO schema_version VALUES (410, 1);
";

struct DirectorySettings;
impl tetrad_settings::SettingsStorage for DirectorySettings {
    fn read(&self, _: tetrad_settings::SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn write(&mut self, _: tetrad_settings::SettingsFileId, _: &[u8]) -> std::io::Result<()> {
        panic!("unexpected settings write")
    }
    fn remove(&mut self, _: tetrad_settings::SettingsFileId) -> std::io::Result<()> {
        panic!("unexpected settings remove")
    }
}

struct DirectoryFixture {
    root: std::path::PathBuf,
    state: tetrad_app::device::State,
    media: MediaLibrary,
}

impl DirectoryFixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "tetrad-directory-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("Presets")).unwrap();
        let mut presets = tetrad_library::stock::StockLibrary::open(
            tetrad_storage::media::MediaRoot::new(&root),
            tetrad_library::stock::StockLimits::default(),
        )
        .unwrap();
        presets.create_setlist("My Presets").unwrap();
        let path = root.join("zenui.db");
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(DIRECTORY_DDL)
            .unwrap();
        let mut database = tetrad_storage::stock::StockDb::open(
            &path,
            tetrad_storage::stock::DbLimits::default(),
            false,
        )
        .unwrap();
        database.transaction(|tx| tx.boot_maintenance()).unwrap();
        let (settings, _) = tetrad_settings::DeviceSettings::load(
            &DirectorySettings,
            tetrad_settings::Hardware::QuadCortex,
        )
        .unwrap();
        let mut state = tetrad_app::device::State::new(
            tetrad_hal::platform::Product::QuadCortex,
            tetrad_hal::platform::Platform::QcOriginal,
            settings,
        );
        state.library = Some(tetrad_app::device::Library {
            presets,
            database: Some(database),
        });
        let media =
            MediaLibrary::new(&FactoryIrCatalog::new(Vec::new()).encode(), cipher()).unwrap();
        Self { root, state, media }
    }

    fn request(&mut self, request: tetrad_ui::DirectoryRequest) {
        let effects = tetrad_app::device::actions::reduce(
            tetrad_app::device::Action::Directory(request.clone()),
            &mut self.state,
        )
        .unwrap();
        let emitted = effects
            .into_iter()
            .find_map(|effect| match effect {
                tetrad_app::device::Effect::Directory(request) => Some(request),
                _ => None,
            })
            .expect("the reducer must emit the real Directory edit");
        assert_eq!(emitted, request);
        tetrad_app::device::media::edit_directory(
            emitted,
            &mut self.state,
            Some(&mut self.media),
            &tetrad_app::device::bringup::RunOptions::STOCK,
        )
        .unwrap();
    }

    fn product(&mut self, kind: tetrad_ui::MediaKind, id: &str) {
        let metadata = br#"{"name":"Original","author":"Player","date":"2026-10-09T09:00:00Z","version":"1","unknown":{"keep":true}}"#;
        self.product_with(kind, id, metadata);
    }

    /// A product with device-encrypted `metadata`, linked into its root folder.
    fn product_with(&mut self, kind: tetrad_ui::MediaKind, id: &str, metadata: &[u8]) {
        use tetrad_storage::stock::{ProductType, StockProduct};
        let mut blob = metadata.to_vec();
        cipher().apply(&mut blob);
        let kind = if kind == tetrad_ui::MediaKind::Capture {
            ProductType::Capture
        } else {
            ProductType::Ir
        };
        let db = self
            .state
            .library
            .as_mut()
            .unwrap()
            .database
            .as_mut()
            .unwrap();
        db.transaction(|tx| {
            tx.put_product(&StockProduct {
                id: id.into(),
                product_type: kind,
                read_only: false,
                raw_data: Some(vec![1, 2, 3, 4]),
                metadata: Some(blob),
            })?;
            let root = tx.root_folder(kind)?.unwrap();
            tx.link(root, id)
        })
        .unwrap();
    }
}

impl Drop for DirectoryFixture {
    fn drop(&mut self) {
        self.state.library = None;
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

/// r17-ui MR 1 alternative: the media side decrypts capture metadata and hands tetrad-ui only
/// the filter and sort fields (`deviceType`, `gainType`, `version`); tetrad-ui holds no cipher.
/// An IR and a negative gain carry none.
#[test]
fn directory_media_carries_the_capture_filter_fields() {
    use tetrad_ui::{CaptureMeta, MediaKind};
    let mut fixture = DirectoryFixture::new();
    let capture = br#"{"name":"Owned","deviceType":"cab","gainType":6,"version":"2"}"#;
    fixture.product_with(MediaKind::Capture, "owned-capture", capture);
    let unset = br#"{"name":"Unset","deviceType":"unknown","gainType":-1,"version":"1"}"#;
    fixture.product_with(MediaKind::Capture, "unset-capture", unset);
    fixture.product_with(MediaKind::Ir, "CIR_owned", br#"{"name":"Owned IR"}"#);
    let library = fixture.state.library.as_ref().unwrap();
    let details = (fixture.media)
        .directory_media(library.database.as_ref())
        .unwrap();
    let capture = |id: &str| {
        let detail = details.iter().find(|detail| detail.value == id).unwrap();
        detail.capture.clone()
    };
    let meta = |device, gain, version: &str| CaptureMeta {
        device,
        gain,
        version: version.into(),
        instrument: None,
    };
    assert_eq!(capture("owned-capture"), Some(meta(Some(4), Some(6), "2")));
    assert_eq!(capture("unset-capture"), Some(meta(None, None, "1")));
    assert_eq!(capture("CIR_owned"), None);
}

/// Health r34 #2: a capture whose metadata does not read leaves the Directory details of the
/// others as they are; the media entries name it once (the device notice).
#[test]
fn one_bad_capture_row_leaves_the_others_in_the_directory_list() {
    use tetrad_ui::MediaKind;
    let mut fixture = DirectoryFixture::new();
    fixture.product_with(MediaKind::Capture, "good-capture", br#"{"name":"Good"}"#);
    fixture.product_with(MediaKind::Capture, "bad-capture", b"not a record");
    let library = fixture.state.library.as_ref().unwrap();
    let details = (fixture.media)
        .directory_media(library.database.as_ref())
        .unwrap();
    let ids: Vec<_> = details.iter().map(|detail| detail.value.as_str()).collect();
    assert_eq!(ids, ["good-capture"]);
    let (_, failures) = fixture.media.entries(library.database.as_ref());
    assert_eq!(
        (failures.iter())
            .filter(|failure| failure.contains("bad-capture"))
            .count(),
        1,
        "{failures:?}"
    );
}

#[test]
fn directory_media_rename_preserves_id_payload_date_and_unknown_encrypted_fields() {
    use tetrad_ui::{DirectoryRequest as R, MediaKind};
    let mut fixture = DirectoryFixture::new();
    for (kind, id) in [
        (MediaKind::Capture, "owned-capture"),
        (MediaKind::Ir, "CIR_owned"),
    ] {
        fixture.product(kind, id);
        let folder = if kind == MediaKind::Capture {
            "local_nc_root"
        } else {
            "local_ir_root"
        };
        let other = format!("{id}-other");
        fixture.product(kind, &other);
        for visit in [id, other.as_str()] {
            fixture.request(R::VisitMedia {
                kind,
                folder: folder.into(),
                id: visit.into(),
            });
        }
        fixture.request(R::RenameMedia {
            kind,
            id: id.into(),
            name: "Renamed".into(),
        });
        // `0x82ddfc`: the renamed item rejoins its recents as the newest entry.
        let recents = if kind == MediaKind::Capture {
            "recents-nc"
        } else {
            "recents-ir"
        };
        assert!(
            std::fs::read_to_string(fixture.root.join(recents))
                .unwrap()
                .starts_with(&format!("{folder}\n{id}\n"))
        );
        let product = fixture
            .state
            .library
            .as_ref()
            .unwrap()
            .database
            .as_ref()
            .unwrap()
            .view()
            .product(id)
            .unwrap()
            .unwrap();
        assert_eq!(product.raw_data, Some(vec![1, 2, 3, 4]));
        let mut metadata = product.metadata.unwrap();
        assert!(serde_json::from_slice::<serde_json::Value>(&metadata).is_err());
        cipher().apply(&mut metadata);
        let json: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
        assert_eq!(json["name"], "Renamed");
        assert_eq!(json["date"], "2026-10-09T09:00:00Z");
        assert_eq!(json["unknown"]["keep"], true);
    }
}

/// re-r31 §2: the capture edit (`0x6901c8` → `0x68f134`) writes the name, `deviceType` and
/// `instrumentType` into the encrypted metadata; the id, rawdata and other keys stay. A name
/// that another user capture holds is refused ("Name already exists") and nothing changes.
#[test]
fn editing_a_capture_rewrites_name_type_and_instrument_and_keeps_other_keys() {
    use tetrad_ui::{CaptureDevice, CaptureInstrument, DirectoryRequest as R, MediaKind};
    let mut fixture = DirectoryFixture::new();
    let stored = br#"{"name":"Original","deviceType":"amp_head","instrumentType":"guitar","date":"2026-10-09T09:00:00Z","unknown":{"keep":true}}"#;
    fixture.product_with(MediaKind::Capture, "edited", stored);
    fixture.product_with(MediaKind::Capture, "other", br#"{"name":"Taken"}"#);
    let metadata = |fixture: &DirectoryFixture| -> serde_json::Value {
        let db = fixture
            .state
            .library
            .as_ref()
            .unwrap()
            .database
            .as_ref()
            .unwrap();
        let product = db.view().product("edited").unwrap().unwrap();
        assert_eq!(product.raw_data, Some(vec![1, 2, 3, 4]));
        let mut metadata = product.metadata.unwrap();
        cipher().apply(&mut metadata);
        serde_json::from_slice(&metadata).unwrap()
    };
    fixture.request(R::EditCapture {
        id: "edited".into(),
        name: "Edited".into(),
        device: Some(CaptureDevice::Cab),
        instrument: Some(CaptureInstrument::Bass),
    });
    let json = metadata(&fixture);
    assert_eq!(
        (&json["name"], &json["deviceType"], &json["instrumentType"]),
        (&"Edited".into(), &"cab".into(), &"bass".into())
    );
    assert_eq!(json["date"], "2026-10-09T09:00:00Z");
    assert_eq!(json["unknown"]["keep"], true);

    let taken = tetrad_app::device::media::edit_directory(
        R::EditCapture {
            id: "edited".into(),
            name: "Taken".into(),
            device: None,
            instrument: Some(CaptureInstrument::Synth),
        },
        &mut fixture.state,
        Some(&mut fixture.media),
        &tetrad_app::device::bringup::RunOptions::STOCK,
    );
    assert!(
        taken.is_err_and(|error| error.to_string().starts_with("Taken already exists")),
        "a name of another capture is refused"
    );
    assert_eq!(metadata(&fixture), json, "a refused edit writes nothing");
}

#[test]
fn directory_recent_and_favorite_media_lists_use_stock_files_and_reach_the_projection() {
    use tetrad_ui::{DirectoryRequest as R, ItemKind, MediaKind};
    let mut fixture = DirectoryFixture::new();
    for (kind, id, folder, file) in [
        (
            MediaKind::Capture,
            "owned-capture",
            "local_nc_root",
            "favorites-nc",
        ),
        (MediaKind::Ir, "CIR_owned", "local_ir_root", "favorites-ir"),
    ] {
        fixture.product(kind, id);
        fixture.request(R::FavoriteMedia {
            kind,
            folder: folder.into(),
            id: id.into(),
            favorite: true,
        });
        let recent = tetrad_app::device::media::recent_media_selection(
            fixture.state.library.as_ref().unwrap().database.as_ref(),
            id,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            recent,
            R::VisitMedia {
                kind,
                folder: folder.into(),
                id: id.into()
            }
        );
        fixture.request(recent);
        assert_eq!(
            std::fs::read_to_string(fixture.root.join(file)).unwrap(),
            format!("{folder}\n{id}\n")
        );
    }
    let preset =
        tetrad_preset::PresetDocument::parse(&[], tetrad_preset::Limits::default()).unwrap();
    let sidecar = tetrad_library::sidecar::Sidecar::new_preset(
        "Owned Preset",
        std::time::SystemTime::UNIX_EPOCH,
        "4.1.0",
    );
    let library = fixture.state.library.as_mut().unwrap();
    let path = library
        .presets
        .store("My Presets", 7, "Owned Preset", &preset, &sidecar)
        .unwrap()
        .value;
    for list in [
        tetrad_library::stock::ItemList::Favorites(tetrad_storage::media::Collection::Presets),
        tetrad_library::stock::ItemList::Recents(tetrad_storage::media::Collection::Presets),
    ] {
        library
            .presets
            .push_item(
                list,
                &tetrad_library::stock::ItemRef {
                    folder: "/media/p4/Presets/My Presets".into(),
                    item: path.clone(),
                },
            )
            .unwrap();
    }
    let library = fixture.state.library.as_ref().unwrap();
    let (entries, errors) = fixture.media.entries(library.database.as_ref());
    assert!(errors.is_empty(), "{errors:?}");
    let details = fixture
        .media
        .directory_media(library.database.as_ref())
        .unwrap();
    let build = |library: &tetrad_app::device::Library| {
        tetrad_app::device::media::directory_view(
            &library.presets,
            library.database.as_ref(),
            &entries,
            &details,
        )
        .unwrap()
    };
    let (view, unread) = build(library);
    assert!(unread.is_empty(), "{unread:?}");
    assert_eq!(view.recents.len(), 3);
    assert!(view.recents.iter().any(|item| item.kind == ItemKind::Preset
        && item.value == path
        && item.name == "Owned Preset"));
    assert!(
        view.favorites
            .iter()
            .any(|item| item.kind == ItemKind::Capture
                && item.name == "Original"
                && item.author == "Player")
    );
    assert!(
        view.recents
            .iter()
            .any(|item| item.kind == ItemKind::Ir && item.date == "2026-10-09T09:00:00Z")
    );
    // Review r11 N18: a preset entry whose folder is gone is skipped (`0x7d0224`). N5: a
    // list that cannot be read is left out with a notice; the rest of the view still builds.
    let gone = tetrad_library::stock::ItemRef {
        folder: "/media/p4/Presets/Gone".into(),
        item: path.clone(),
    };
    let recents =
        tetrad_library::stock::ItemList::Recents(tetrad_storage::media::Collection::Presets);
    let library = fixture.state.library.as_mut().unwrap();
    library.presets.push_item(recents, &gone).unwrap();
    std::fs::remove_file(fixture.root.join("recents-ir")).unwrap();
    std::fs::create_dir(fixture.root.join("recents-ir")).unwrap();
    let (later, unread) = build(fixture.state.library.as_ref().unwrap());
    assert_eq!(unread.len(), 1, "{unread:?}");
    let kinds: Vec<_> = later.recents.iter().map(|item| item.kind).collect();
    assert_eq!(kinds, [ItemKind::Preset, ItemKind::Capture]);
    std::fs::remove_dir(fixture.root.join("recents-ir")).unwrap();
    fixture.request(R::FavoriteMedia {
        kind: MediaKind::Capture,
        folder: "local_nc_root".into(),
        id: "owned-capture".into(),
        favorite: false,
    });
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("favorites-nc")).unwrap(),
        ""
    );
}

/// Review r11 N18: `0x7d0224` keeps a capture or IR entry only when its folder key names a
/// folder (`0x76bc68`: the local root, or `0x766a20` `"<id>_q"` for a zenui.db folder) that
/// holds the item (`0x83a268`).
#[test]
fn media_entries_are_kept_only_when_their_folder_key_holds_the_item() {
    use tetrad_library::stock::{ItemList, ItemRef};
    use tetrad_storage::media::Collection;
    use tetrad_storage::stock::{FolderType, NewFolder, ProductType};
    use tetrad_ui::MediaKind;
    let mut fixture = DirectoryFixture::new();
    fixture.product(MediaKind::Capture, "linked");
    let library = fixture.state.library.as_mut().unwrap();
    let db = library.database.as_mut().unwrap();
    let (root, ir_root, empty) = db
        .transaction(|tx| {
            let root = tx.root_folder(ProductType::Capture)?.unwrap();
            let empty = tx.create_folder(&NewFolder {
                product_type: ProductType::Capture,
                folder_type: FolderType::User,
                name: "Empty",
                parent: root,
                read_only: false,
            })?;
            Ok((root, tx.root_folder(ProductType::Ir)?.unwrap(), empty))
        })
        .unwrap();
    let keys = [
        "local_nc_root".to_owned(),
        format!("{root}_q"),
        format!("{empty}_q"),
        format!("{ir_root}_q"),
        format!("{root}_f"),
        format!("0{root}_q"),
        "local_ir_root".to_owned(),
    ];
    for folder in keys.iter().rev() {
        let entry = ItemRef {
            folder: folder.clone(),
            item: "linked".into(),
        };
        let list = ItemList::Favorites(Collection::Captures);
        library.presets.push_item(list, &entry).unwrap();
    }
    let library = fixture.state.library.as_ref().unwrap();
    let (entries, _) = fixture.media.entries(library.database.as_ref());
    let details = (fixture.media)
        .directory_media(library.database.as_ref())
        .unwrap();
    let (view, _) = tetrad_app::device::media::directory_view(
        &library.presets,
        library.database.as_ref(),
        &entries,
        &details,
    )
    .unwrap();
    let kept: Vec<_> = view.favorites.iter().map(|item| &item.folder).collect();
    assert_eq!(kept, [&keys[0], &keys[1]]);
}

#[test]
fn directory_folder_create_and_rename_persist_and_protect_the_user_root() {
    use tetrad_ui::{DirectoryRequest as R, MediaKind};
    let mut fixture = DirectoryFixture::new();
    let root = fixture
        .state
        .library
        .as_ref()
        .unwrap()
        .database
        .as_ref()
        .unwrap()
        .view()
        .root_folder(tetrad_storage::stock::ProductType::Capture)
        .unwrap()
        .unwrap();
    fixture.request(R::CreateFolder {
        kind: MediaKind::Capture,
        parent: root,
        name: "Folder A".into(),
    });
    let folders = fixture
        .state
        .library
        .as_ref()
        .unwrap()
        .database
        .as_ref()
        .unwrap()
        .view()
        .folders()
        .unwrap();
    let id = folders
        .iter()
        .find(|folder| folder.name == "Folder A")
        .unwrap()
        .id;
    fixture.request(R::RenameFolder {
        id,
        name: "Folder B".into(),
    });
    let db = fixture
        .state
        .library
        .as_ref()
        .unwrap()
        .database
        .as_ref()
        .unwrap();
    assert!(
        db.view()
            .folders()
            .unwrap()
            .iter()
            .any(|folder| folder.id == id && folder.name == "Folder B" && folder.parent == root)
    );
    let error = tetrad_app::device::media::edit_directory(
        R::DeleteFolder {
            id: root,
            remove_products: false,
        },
        &mut fixture.state,
        Some(&mut fixture.media),
        &tetrad_app::device::bringup::RunOptions::STOCK,
    )
    .unwrap_err();
    assert!(error.to_string().contains("read-only"));
}

#[test]
fn directory_folder_delete_keeps_products_unless_the_user_selects_remove_products() {
    use tetrad_storage::stock::ProductType;
    use tetrad_ui::{DirectoryRequest as R, MediaKind};
    let mut fixture = DirectoryFixture::new();
    fixture.product(MediaKind::Ir, "CIR_owned");
    let db = fixture
        .state
        .library
        .as_mut()
        .unwrap()
        .database
        .as_mut()
        .unwrap();
    let root = db.view().root_folder(ProductType::Ir).unwrap().unwrap();
    let first =
        tetrad_library::stock::create_media_folder(db, ProductType::Ir, root, "Folder A").unwrap();
    let second =
        tetrad_library::stock::create_media_folder(db, ProductType::Ir, root, "Folder B").unwrap();
    db.transaction(|tx| {
        tx.link(first, "CIR_owned")?;
        tx.link(second, "CIR_owned")
    })
    .unwrap();
    fixture.request(R::DeleteFolder {
        id: first,
        remove_products: false,
    });
    assert!(
        fixture
            .state
            .library
            .as_ref()
            .unwrap()
            .database
            .as_ref()
            .unwrap()
            .view()
            .product("CIR_owned")
            .unwrap()
            .is_some()
    );
    fixture.request(R::DeleteFolder {
        id: second,
        remove_products: true,
    });
    let view = fixture
        .state
        .library
        .as_ref()
        .unwrap()
        .database
        .as_ref()
        .unwrap()
        .view();
    assert!(view.product("CIR_owned").unwrap().is_none());
    assert!(view.folder_products(root).unwrap().is_empty());
}

#[test]
fn directory_media_delete_removes_every_membership_and_favorite_recent_reference() {
    use tetrad_ui::{DirectoryRequest as R, MediaKind};
    let mut fixture = DirectoryFixture::new();
    for kind in [MediaKind::Capture, MediaKind::Ir] {
        let (id, folder, collection) = if kind == MediaKind::Capture {
            (
                "owned-capture",
                "local_nc_root",
                tetrad_storage::media::Collection::Captures,
            )
        } else {
            (
                "CIR_owned",
                "local_ir_root",
                tetrad_storage::media::Collection::Irs,
            )
        };
        fixture.product(kind, id);
        fixture.request(R::FavoriteMedia {
            kind,
            id: id.into(),
            folder: folder.into(),
            favorite: true,
        });
        fixture.request(R::VisitMedia {
            kind,
            id: id.into(),
            folder: folder.into(),
        });
        fixture.request(R::DeleteMedia {
            kind,
            id: id.into(),
        });
        let library = fixture.state.library.as_ref().unwrap();
        assert!(
            library
                .database
                .as_ref()
                .unwrap()
                .view()
                .product(id)
                .unwrap()
                .is_none()
        );
        assert!(
            library
                .presets
                .items(tetrad_library::stock::ItemList::Favorites(collection))
                .unwrap()
                .is_empty()
        );
        assert!(
            library
                .presets
                .items(tetrad_library::stock::ItemList::Recents(collection))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn directory_bringup_refuses_every_new_write_before_any_storage_access_and_audition_is_a_slotless_load()
 {
    use tetrad_ui::{DirectoryRequest as R, ItemKind, MediaKind};
    let mut fixture = DirectoryFixture::new();
    fixture.state.library = None;
    let options = tetrad_app::device::bringup::RunOptions::bringup(
        tetrad_app::device::bringup::Stage::Full,
        std::time::Duration::from_secs(60),
        fixture.root.join("logs"),
    );
    for request in [
        R::RenameSetlist {
            from: "Live".into(),
            to: "Tour".into(),
        },
        R::DeleteSetlist {
            name: "Tour".into(),
        },
        R::MovePreset {
            setlist: "Tour".into(),
            from: 0,
            to: 1,
            overwrite: true,
        },
        R::CreateFolder {
            kind: MediaKind::Capture,
            parent: 1,
            name: "New Folder".into(),
        },
        R::RenameFolder {
            id: 1,
            name: "Renamed".into(),
        },
        R::DeleteFolder {
            id: 1,
            remove_products: true,
        },
        R::RenameMedia {
            kind: MediaKind::Ir,
            id: "CIR_owned".into(),
            name: "Renamed".into(),
        },
        R::DeleteMedia {
            kind: MediaKind::Capture,
            id: "owned-capture".into(),
        },
        R::FavoriteMedia {
            kind: MediaKind::Ir,
            folder: "local_ir_root".into(),
            id: "CIR_owned".into(),
            favorite: true,
        },
        R::FavoritePreset {
            path: "/media/p4/Presets/Tour/Lead.pb".into(),
            favorite: true,
        },
    ] {
        let effects =
            tetrad_app::device::media::edit_directory(request, &mut fixture.state, None, &options)
                .unwrap();
        assert!(
            matches!(&effects[..], [tetrad_app::device::Effect::Notice(tetrad_app::device::Subsystem::Platform, message)] if message.contains("without --persist"))
        );
    }
    // A media recent follows an edit: skipped with no notice, as the preset recent is
    // (review-w6 F17).
    let recent = R::VisitMedia {
        kind: MediaKind::Capture,
        folder: "local_nc_root".into(),
        id: "owned-capture".into(),
    };
    let effects =
        tetrad_app::device::media::edit_directory(recent, &mut fixture.state, None, &options)
            .unwrap();
    assert!(effects.is_empty(), "{effects:?}");
    // Stock auditions only Downloads and Plugin presets (`0x7c2974`), as a load with no
    // setlist slot; it is not a library write.
    let audition = |id: &str, kind| R::Audition {
        kind,
        folder: String::new(),
        id: id.into(),
    };
    let error = tetrad_app::device::media::edit_directory(
        audition("owned-capture", ItemKind::Capture),
        &mut fixture.state,
        None,
        &options,
    )
    .unwrap_err();
    assert!(error.to_string().contains("Downloads and Plugin"));
    assert!(
        tetrad_app::device::media::edit_directory(
            audition("/media/p4/Presets/Tour/Lead.pb", ItemKind::Preset),
            &mut fixture.state,
            None,
            &options,
        )
        .is_err()
    );
    let plugin = "/opt/neuraldsp/Plugins/Owned/Neural DSP/Clean.pb";
    assert_eq!(
        tetrad_app::device::media::edit_directory(
            audition(plugin, ItemKind::Preset),
            &mut fixture.state,
            None,
            &options,
        )
        .unwrap(),
        [tetrad_app::device::Effect::AuditionPreset(plugin.into())]
    );
}

/// Lane controls and a cab clone whose `ir selector` (index 0) names an IR.
const MODELS: &str = "<Models>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' defaultValue='0'/></Model></Category>\
<Category id='11' name='Mixer' hidden='true'><Model id='11000' name='Mixer'>\
<Parameter name='LEVEL A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/>\
</Model></Category>\
<Category id='12' name='Cab'><Model id='12000' name='Base Cab' blob='cab'>\
<Parameter name='A' type='float' min='0' max='1' defaultValue='0'/></Model>\
<Model id='12001' name='Clone Cab' clones='12000'>\
<Parameter name='ir selector' type='string' defaultValue='IR' replaces='0'/></Model></Category>\
<Category id='23' name='Lane Output' hidden='true'><Model id='23000' name='Lane Output'>\
<Parameter name='VOLUME' type='float' min='0' max='1' defaultValue='0.76923'/>\
<Parameter name='PAN' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>\
<Category id='25' name='Tempo' hidden='true'><Model id='25000' name='Tempo'>\
<Parameter name='TEMPO' type='float' min='40' max='240' defaultValue='120'/></Model></Category>\
<Category id='28' name='Input Gate' hidden='true'><Model id='28000' name='Input Gate'>\
<Parameter name='THRESHOLD' type='float' min='-96' max='0' defaultValue='-60'/></Model></Category>\
</Models>";

const REPORT: &str = "<ModuleReport>\
<Kernel core='0' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='1' cpu='2' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='2' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='3' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Module id='12000' name='Base Cab' blob_name='cab' category='12' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
</ModuleReport>";

impl DirectoryFixture {
    /// The catalog and the new-preset shape, loaded from `path`.
    fn load(&mut self, path: &str) -> tetrad_catalog::Specs {
        let specs = tetrad_catalog::Specs::resolve(
            &tetrad_catalog::Catalog::parse(MODELS, REPORT).unwrap(),
        )
        .unwrap();
        let presets = &self.state.library.as_ref().unwrap().presets;
        let read = tetrad_app::device::preset::read(presets, None, &specs).unwrap();
        let prepared = tetrad_app::device::preset::prepare(read, &specs, false, None).unwrap();
        *self.state.current.get_mut() = Some(tetrad_app::device::Current {
            path: Some(path.into()),
            preset: prepared.preset,
            migration: prepared.migration,
            deployment: prepared.deployment,
        });
        self.state.specs = Some(specs.clone());
        specs
    }
}

/// `0x5b57b8` → `0x780e1c`: preset ± on an auditioned plugin preset auditions its folder
/// neighbour, wrapping at both ends, through the gated Directory request; a setlist preset
/// steps the setlist (`None`).
#[test]
fn preset_step_walks_the_folder_of_an_auditioned_plugin_preset() {
    let mut fixture = DirectoryFixture::new();
    let plugin = fixture.root.join("Plugins/Owned");
    std::fs::create_dir_all(&plugin).unwrap();
    for name in ["A.pb", "B.pb", "C.pb"] {
        std::fs::write(plugin.join(name), b"owned").unwrap();
    }
    fixture.load("/media/p4/Plugins/Owned/A.pb");
    let step = |state: &tetrad_app::device::State, step| {
        tetrad_app::device::media::audition_step(state, step).map(|effects| {
            effects
                .into_iter()
                .map(|effect| match effect {
                    tetrad_app::device::Effect::Directory(
                        tetrad_ui::DirectoryRequest::Audition { id, .. },
                    ) => id,
                    other => panic!("unexpected {other:?}"),
                })
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(
        step(&fixture.state, -1),
        Some(vec!["/media/p4/Plugins/Owned/C.pb".to_owned()])
    );
    assert_eq!(
        step(&fixture.state, 1),
        Some(vec!["/media/p4/Plugins/Owned/B.pb".to_owned()])
    );
    fixture.state.current.get_mut().as_mut().unwrap().path =
        Some("/media/p4/Presets/My Presets/Lead.pb".into());
    assert_eq!(step(&fixture.state, 1), None);
}

/// r28-ui MR 4 (`0x78d204` → `0x5b37f0`, index -1): a tap on a stored DOWNLOADS preset
/// auditions it from `downloads_product_preset` by its product id; its path names no file
/// and counts as an audition (no recent, no MIDI PC). A product not stored does not read.
/// r33-ui MR D: preset ± walks the stored DOWNLOADS presets in table order, wrapping.
#[test]
fn a_stored_downloads_preset_auditions_from_its_table() {
    use tetrad_app::device::bringup::RunOptions;
    use tetrad_app::device::media::{auditioned_path, edit_directory, read_download};
    use tetrad_storage::stock::{Download, DownloadKind};
    let mut fixture = DirectoryFixture::new();
    let specs = common::STOCK.specs();
    let mut preset = common::light_preset(&specs, 0);
    preset.name = Some("Lead".into());
    let row = Download {
        id: "p-1".into(),
        hash: None,
        raw_data: Some(preset.to_bytes()),
        metadata: Some(br#"{"name":"Lead"}"#.to_vec()),
    };
    let db = fixture.state.library.as_mut().unwrap().database.as_mut();
    let rows = ["p-1", "p-5", "p-6"].map(|id| Download {
        id: id.into(),
        ..row.clone()
    });
    (db.unwrap().transaction(|write| {
        (rows.iter()).try_for_each(|row| write.put_download(DownloadKind::Preset, row))
    }))
    .unwrap();
    let audition = |id: &str| tetrad_ui::DirectoryRequest::Audition {
        kind: tetrad_ui::ItemKind::Preset,
        folder: "cloud-0-1".into(),
        id: id.into(),
    };
    let effects = edit_directory(
        audition("p-1"),
        &mut fixture.state,
        Some(&mut fixture.media),
        &RunOptions::STOCK,
    );
    let path = "cloud-0-1/p-1";
    assert_eq!(
        effects.unwrap(),
        [tetrad_app::device::Effect::AuditionPreset(path.into())]
    );
    assert!(auditioned_path(path));
    let read = read_download(&fixture.state, path).expect("a Downloads path");
    assert_eq!(read.unwrap().name.as_deref(), Some("Lead"));
    assert!(
        read_download(&fixture.state, "cloud-0-1/p-2")
            .unwrap()
            .is_err()
    );
    assert!(read_download(&fixture.state, "/media/p4/Presets/My Presets/Lead.pb").is_none());
    fixture.load(path);
    let step = |state: &tetrad_app::device::State, step| {
        tetrad_app::device::media::audition_step(state, step).map(|effects| effects.to_vec())
    };
    let effect = |id: &str| tetrad_app::device::Effect::Directory(audition(id));
    assert_eq!(step(&fixture.state, -1), Some(vec![effect("p-6")]));
    assert_eq!(step(&fixture.state, 1), Some(vec![effect("p-5")]));
}

/// `0x59dd78`: a pasted (or added, placed, changed) block records the library IR it names
/// as a media recent, filed under the IR root.
#[test]
fn pasted_block_records_its_ir_as_a_media_recent() {
    use tetrad_app::device::actions::LayoutEdit;
    let mut fixture = DirectoryFixture::new();
    fixture.product(tetrad_ui::MediaKind::Ir, "CIR_owned");
    let specs = fixture.load("/media/p4/Presets/My Presets/Lead.pb");
    let scene = fixture.state.current.as_ref().unwrap().deployment.scene();
    let mut block = tetrad_engine::new_instance(specs.model(12001).unwrap());
    block.set_text(0, scene, "CIR_owned");
    let slot = tetrad_engine::Slot {
        row: tetrad_engine::Row::new(0).unwrap(),
        column: tetrad_engine::Column::new(0).unwrap(),
    };
    let effects = tetrad_app::device::actions::reduce(
        tetrad_app::device::Action::EditLayout(LayoutEdit::Paste {
            slot,
            block: Box::new(block),
        }),
        &mut fixture.state,
    )
    .unwrap();
    assert!(effects.contains(&tetrad_app::device::Effect::Directory(
        tetrad_ui::DirectoryRequest::VisitMedia {
            kind: tetrad_ui::MediaKind::Ir,
            folder: "local_ir_root".into(),
            id: "CIR_owned".into(),
        }
    )));
}

/// r16-media boot safety: in a bring-up run no boot step that changes user data runs, also
/// with `--persist` (review bring-up safety 2 R2); without it the new folder-menu writes are
/// refused too. A stock run applies the `zenui.db` patch chain once, logs one notice, and a
/// second boot changes nothing.
#[test]
fn boot_data_steps_never_run_in_bringup_and_the_database_upgrade_runs_once() {
    use tetrad_app::device::bringup::{BootDataStep, RunOptions, Stage};
    use tetrad_app::device::{Action, Effect};
    let fixture = DirectoryFixture::new();
    let bringup = RunOptions::bringup(
        Stage::Full,
        std::time::Duration::from_secs(60),
        fixture.root.join("logs"),
    );
    // Review bring-up safety 2 R2: `--persist` does not allow them either (they write
    // markers in `/etc` and `/opt/neuraldsp`).
    let persist = RunOptions {
        persist: true,
        ..bringup.clone()
    };
    for step in [
        BootDataStep::DatabaseUpgrade,
        BootDataStep::LegacyIrs,
        BootDataStep::LegacyCaptures,
        BootDataStep::CaptureStoreTranslation,
        BootDataStep::LegacyCaptureFiles,
        BootDataStep::CaptureCorruptionCheck,
    ] {
        assert!(bringup.boot_refusal(step).is_some(), "{step:?}");
        assert!(persist.boot_refusal(step).is_some(), "{step:?}");
        assert!(RunOptions::STOCK.boot_refusal(step).is_none(), "{step:?}");
    }
    for action in [
        Action::DuplicateMediaFolder { folder: 2 },
        Action::SetSavingDestination { folder: 2 },
    ] {
        assert!(bringup.refusal(&Effect::Action(action)).is_some());
    }

    let old = fixture.root.join("old.db");
    rusqlite::Connection::open(&old)
        .unwrap()
        .execute_batch(
            "CREATE TABLE product_preset (id TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY (id));
             CREATE TABLE product_nc (id TEXT, hash TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY (id));",
        )
        .unwrap();
    let before = std::fs::read(&old).unwrap();
    for options in [&bringup, &persist] {
        assert_eq!(
            tetrad_app::device::media::upgrade_database(&old, options),
            None
        );
    }
    assert_eq!(std::fs::read(&old).unwrap(), before);
    let notice = tetrad_app::device::media::upgrade_database(&old, &RunOptions::STOCK).unwrap();
    assert!(notice.contains("300.1, 300.2, 410.1"), "{notice}");
    assert_eq!(
        tetrad_app::device::media::upgrade_database(&old, &RunOptions::STOCK),
        None
    );
    tetrad_storage::stock::StockDb::open(&old, tetrad_storage::stock::DbLimits::default(), false)
        .unwrap();
}

/// Folder menu (manual M819, M821): DUPLICATE copies under the next free name; SET AS SAVING
/// DESTINATION stores the stock folder key, which `saving_destination` reads back.
#[test]
fn folder_menu_duplicates_and_sets_the_saving_destination() {
    use tetrad_app::device::Action;
    use tetrad_storage::stock::ProductType;
    let mut fixture = DirectoryFixture::new();
    let db = |fixture: &DirectoryFixture| {
        let view = fixture
            .state
            .library
            .as_ref()
            .unwrap()
            .database
            .as_ref()
            .unwrap();
        (
            view.view().root_folder(ProductType::Ir).unwrap().unwrap(),
            view.view().folders().unwrap(),
        )
    };
    let (root, _) = db(&fixture);
    fixture.request(tetrad_ui::DirectoryRequest::CreateFolder {
        kind: tetrad_ui::MediaKind::Ir,
        parent: root,
        name: "Cabs".into(),
    });
    let (_, folders) = db(&fixture);
    let cabs = folders
        .iter()
        .find(|folder| folder.name == "Cabs")
        .unwrap()
        .id;
    tetrad_app::device::actions::reduce(
        Action::DuplicateMediaFolder { folder: cabs },
        &mut fixture.state,
    )
    .unwrap();
    let (_, folders) = db(&fixture);
    assert!(
        folders
            .iter()
            .any(|folder| folder.name == "Cabs 2" && folder.parent == root)
    );
    for (folder, key) in [
        (cabs, format!("{cabs}_q")),
        (root, "local_ir_root".to_owned()),
    ] {
        tetrad_app::device::actions::reduce(
            Action::SetSavingDestination { folder },
            &mut fixture.state,
        )
        .unwrap();
        let stored = fixture
            .state
            .settings
            .settings()
            .library
            .download_destination_ir
            .clone();
        assert_eq!(stored.as_deref(), Some(key.as_str()));
        let view = fixture
            .state
            .library
            .as_ref()
            .unwrap()
            .database
            .as_ref()
            .unwrap()
            .view();
        let resolved =
            tetrad_library::stock::saving_destination(&view, ProductType::Ir, stored.as_deref());
        assert_eq!(resolved.unwrap(), folder);
    }
}

/// A legacy `My Captures` directory under the sim `/media/p4`: `with.cns` and its `.json`,
/// and `bare.cns` without metadata.
fn legacy_captures(media: &common::Media) -> std::path::PathBuf {
    let legacy = media.0.join("Captures/My Captures");
    std::fs::create_dir_all(&legacy).unwrap();
    for (file, bytes) in [
        ("with.cns", &b"capture"[..]),
        ("with.json", b"{}"),
        ("bare.cns", b"capture"),
    ] {
        std::fs::write(legacy.join(file), bytes).unwrap();
    }
    legacy
}

fn library_notices(log: &std::cell::RefCell<common::Log>, text: &str) -> usize {
    (log.borrow().notices.iter())
        .filter(|notice| notice.detail.contains(text))
        .count()
}

/// r16-media boot step (`0x6e71cc`): a stock boot moves each legacy capture with metadata
/// into `zenui.db` once, renames the directory to `_legacy` with the skipped file inside and
/// logs one notice; the next boot changes nothing and logs none.
#[test]
fn legacy_captures_migrate_once_on_boot() {
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let legacy = legacy_captures(&media);
    let (device, log, media) = common::boot(media);
    assert_eq!(
        library_notices(&log, "legacy captures: 1 moved"),
        1,
        "{:?}",
        log.borrow().notices
    );
    assert!(!legacy.exists());
    assert!(
        media
            .0
            .join("Captures/My Captures_legacy/bare.cns")
            .exists()
    );
    let db = device.state().library.as_ref().unwrap().database.as_ref();
    assert!(db.unwrap().view().product("with").unwrap().is_some());
    drop(device);
    let (_device, log, _media) = common::boot(media);
    assert_eq!(library_notices(&log, "legacy captures"), 0);
}

/// A failed migration keeps the user data: with the `_legacy` copy already present the step
/// stops before any change, both directories keep their files, `zenui.db` gets no row, and
/// the notice says the captures wait for the next boot.
#[test]
fn failed_legacy_capture_migration_keeps_every_file() {
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let legacy = legacy_captures(&media);
    let copy = media.0.join("Captures/My Captures_legacy");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::write(copy.join("old.cns"), b"capture").unwrap();
    let (device, log, _media) = common::boot(media);
    assert_eq!(
        library_notices(&log, "legacy captures kept for the next boot"),
        1
    );
    assert!(legacy.join("with.cns").exists() && legacy.join("bare.cns").exists());
    assert!(copy.join("old.cns").exists());
    let db = device.state().library.as_ref().unwrap().database.as_ref();
    assert!(db.unwrap().view().product("with").unwrap().is_none());
}

/// Review r18 F2, stock boot order (`0x7044b8` → `0x6a6f78` before `0x6e72d4`/`0x6e71cc`): the
/// capture store translation runs before the legacy migrations. An unflagged store is left as
/// it is with no seed read (stock translates only a global store), and the legacy capture
/// lands after it as it is stored, with no cipher pass. The boot also logs no pending DSP
/// step.
#[test]
fn boot_translates_the_capture_store_before_the_legacy_captures_land() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_storage::stock::{CaptureEncryption, CaptureStore};
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    legacy_captures(&media);
    let log = Rc::new(RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher());
    // A 4.1.0 unit: the corruption check is done (the synthetic capture would not load).
    sim_files(
        &mut sim,
        &[CAPTURE_CORRUPTION_CHECKED, CAPTURE_CONVERSION_FINISHED],
    );
    let device = tetrad_app::device::Device::start(sim).unwrap();
    assert!(device.state().library.is_some());
    assert_eq!(library_notices(&log, "no ZenUI seed"), 0);
    // Review r18 F13: the globals leave no pending step (the `0x56` gains go out from CAL_*).
    assert_eq!(library_notices(&log, "not done"), 0);
    let db = device.state().library.as_ref().unwrap().database.as_ref();
    let view = db.unwrap().view();
    assert_eq!(
        view.capture_encryption(CaptureStore::User).unwrap(),
        CaptureEncryption::Unset
    );
    let row = view.product("with").unwrap().unwrap();
    assert_eq!(
        (row.raw_data.as_deref(), row.metadata.as_deref()),
        (Some(&b"capture"[..]), Some(&b"{}"[..]))
    );
}

/// Review r18 F4: a factory (read-only) capture is never deleted or renamed, also when Cortex
/// Control sends its id straight through (File item DELETE in a root folder).
#[test]
fn directory_media_edits_refuse_read_only_products() {
    use tetrad_storage::stock::{ProductType, StockProduct};
    use tetrad_ui::{DirectoryRequest as R, MediaKind};
    let mut fixture = DirectoryFixture::new();
    let db = (fixture.state.library.as_mut())
        .and_then(|library| library.database.as_mut())
        .unwrap();
    let mut metadata = br#"{"name":"Factory"}"#.to_vec();
    cipher().apply(&mut metadata);
    db.transaction(|tx| {
        tx.put_product(&StockProduct {
            id: "factory-capture".into(),
            product_type: ProductType::Capture,
            read_only: true,
            raw_data: Some(vec![1, 2, 3, 4]),
            metadata: Some(metadata.clone()),
        })
    })
    .unwrap();
    for request in [
        R::DeleteMedia {
            kind: MediaKind::Capture,
            id: "factory-capture".into(),
        },
        R::RenameMedia {
            kind: MediaKind::Capture,
            id: "factory-capture".into(),
            name: "Renamed".into(),
        },
    ] {
        let edited = tetrad_app::device::media::edit_directory(
            request,
            &mut fixture.state,
            Some(&mut fixture.media),
            &tetrad_app::device::bringup::RunOptions::STOCK,
        );
        assert!(edited.is_err());
    }
    let db = (fixture.state.library.as_ref())
        .and_then(|library| library.database.as_ref())
        .unwrap();
    let row = db.view().product("factory-capture").unwrap().unwrap();
    assert_eq!(row.metadata, Some(metadata));
}

/// The sim's service files (shared with the device) with empty `markers`. A 4.1.0 rootfs has
/// both "done" markers (`CAPTURE_CORRUPTION_CHECKED`, `CAPTURE_CONVERSION_FINISHED`).
fn sim_files(sim: &mut common::Sim, markers: &[&str]) -> tetrad_services::fake::MemFiles {
    use tetrad_app::device::platform::{Platform as _, ServiceBackends as _};
    let files = sim.services().files().clone();
    for marker in markers {
        files.put(marker, "");
    }
    files
}

/// r23-media, the boot after a restore (`0x704198`): with the translation flag set and no
/// seed-only cipher, the legacy capture files are not translated, so the legacy import waits
/// (no row, no rename, the flag stays). The conversion step waits too (r25-media: its input
/// is still in the seed-only cipher): no marker, the capture without a sidecar stays.
#[test]
fn a_pending_capture_translation_holds_the_legacy_import() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tetrad_app::device::media::{
        CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED, CAPTURE_TRANSLATION_FLAG,
    };
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let legacy = legacy_captures(&media);
    let log = Rc::new(RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher());
    let files = sim_files(
        &mut sim,
        &[CAPTURE_CORRUPTION_CHECKED, CAPTURE_TRANSLATION_FLAG],
    );
    let device = tetrad_app::device::Device::start(sim).unwrap();

    assert_eq!(library_notices(&log, "wait for their translation"), 1);
    assert_eq!(files.get(CAPTURE_TRANSLATION_FLAG), Some(Vec::new()));
    assert!(legacy.join("with.cns").exists() && legacy.join("bare.cns").exists());
    let db = device.state().library.as_ref().unwrap().database.as_ref();
    assert!(db.unwrap().view().product("with").unwrap().is_none());
    assert!(files.get(CAPTURE_CONVERSION_FINISHED).is_none());
}

/// A synthetic format-0 capture (`0x6fa77c`) around a 0x2864-byte 1→2→2→1 V1 block.
fn format_zero_capture() -> Vec<u8> {
    let mut block = Vec::new();
    let mut words = |words: &[u32]| block.extend(words.iter().flat_map(|word| word.to_le_bytes()));
    words(&[48_000f32.to_bits(), 512, 2048]);
    words(&[0.25f32.to_bits(); 2560]);
    words(&[1f32.to_bits(), 1f32.to_bits(), 3, 1, 2, 2, 1]);
    words(&[0.5f32.to_bits(); 13]);
    words(&[20f32.to_bits(), 0.7f32.to_bits()]);
    let mut out = b"CNS\0\0\0\0".to_vec();
    out.extend(
        [0u32, 0, 0, 0, 0]
            .iter()
            .flat_map(|word| word.to_be_bytes()),
    );
    let header = out.len() - 7;
    out[0xb..0xf].copy_from_slice(&(header as u32).to_be_bytes());
    out.extend(0x2864u32.to_be_bytes());
    out.extend(block);
    out
}

/// r25-media, `0x702d58` at boot without its marker: a format-0 legacy capture is converted
/// before the legacy import (`0x704198` step 4 before `0x6e71cc`), so it lands in the library
/// as its `<hash>` capture and loads as V1; the old file is gone, the owned journal is
/// removed and the marker is written.
#[test]
fn boot_converts_a_format_zero_capture_before_the_legacy_import() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tetrad_app::device::media::{
        CAPTURE_CONVERSION_FINISHED, CAPTURE_CONVERSION_JOURNAL, CAPTURE_CORRUPTION_CHECKED,
    };
    use tetrad_storage::stock::ProductType;
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let legacy = media.0.join("Captures/My Captures");
    std::fs::create_dir_all(&legacy).unwrap();
    let mut old = format_zero_capture();
    cipher().apply(&mut old);
    std::fs::write(legacy.join("Old.cns"), old).unwrap();
    let log = Rc::new(RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher());
    let files = sim_files(&mut sim, &[CAPTURE_CORRUPTION_CHECKED]);
    let device = tetrad_app::device::Device::start(sim).unwrap();

    assert_eq!(
        library_notices(&log, "converted to the current format: 1"),
        1,
        "{:?}",
        log.borrow().notices
    );
    assert!(files.get(CAPTURE_CONVERSION_FINISHED).is_some());
    assert!(files.get(CAPTURE_CONVERSION_JOURNAL).is_none());
    assert!(!media.0.join("Captures/My Captures_legacy/Old.cns").exists());
    let library = device.state().library.as_ref().unwrap();
    let view = library.database.as_ref().unwrap().view();
    let ids = view.product_ids(ProductType::Capture).unwrap();
    let [id] = ids.as_slice() else {
        panic!("one converted capture: {ids:?}");
    };
    assert_eq!(id.len(), 64, "the id is the block's SHA-256");
    let row = view.product(id).unwrap().unwrap();
    let catalog = FactoryIrCatalog::new(Vec::new()).encode();
    assert_eq!(
        (MediaLibrary::new(&catalog, cipher()).unwrap())
            .check_capture(&row.raw_data.unwrap(), &row.metadata.unwrap()),
        tetrad_library::stock::CaptureCheck::Sound
    );
}

/// Review r25 Q1: a journal entry whose old file and converted pair are both gone (a capture
/// the user removed after a cut run) is dropped. Before, it failed "file missing" at every
/// boot: no marker, and the legacy import waited, so no legacy capture reached the library.
#[test]
fn a_stale_conversion_journal_entry_does_not_hold_the_legacy_captures() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tetrad_app::device::media::{
        CAPTURE_CONVERSION_FINISHED, CAPTURE_CONVERSION_JOURNAL, CAPTURE_CORRUPTION_CHECKED,
    };
    use tetrad_storage::stock::ProductType;
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let legacy = media.0.join("Captures/My Captures");
    std::fs::create_dir_all(&legacy).unwrap();
    let mut old = format_zero_capture();
    cipher().apply(&mut old);
    std::fs::write(legacy.join("Old.cns"), old).unwrap();
    let log = Rc::new(RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher());
    let files = sim_files(&mut sim, &[CAPTURE_CORRUPTION_CHECKED]);
    files.put(
        CAPTURE_CONVERSION_JOURNAL,
        format!(r#"{{"Gone.cns":"{}"}}"#, "0".repeat(64)),
    );
    let device = tetrad_app::device::Device::start(sim).unwrap();

    assert!(
        files.get(CAPTURE_CONVERSION_FINISHED).is_some(),
        "{:?}",
        log.borrow().notices
    );
    assert!(files.get(CAPTURE_CONVERSION_JOURNAL).is_none());
    let library = device.state().library.as_ref().unwrap();
    let view = library.database.as_ref().unwrap().view();
    assert_eq!(view.product_ids(ProductType::Capture).unwrap().len(), 1);
}

/// r23-media, `0x702f70` at boot without its marker: a user capture whose metadata does not
/// read leaves the library for the quarantine table (stock deletes it), its name reaches the
/// Corrupt Captures list once, and the marker is written so the next boot does not check again.
/// A sound capture (a legacy one the same boot converts) sits beside it: alone, the bad row
/// would be held (review r25 Q7).
#[test]
fn the_corruption_check_quarantines_a_bad_capture_and_names_it_once() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tetrad_app::device::media::CAPTURE_CORRUPTION_CHECKED;
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, ProductType, StockDb, StockProduct,
    };
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| {
        write.put_product(&StockProduct {
            id: "broken".into(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(vec![1, 2, 3]),
            metadata: Some(b"not encrypted JSON".to_vec()),
        })?;
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
        write.set_capture_encryption(CaptureStore::Downloads, CaptureEncryption::Local)
    })
    .unwrap();
    drop(db);
    let legacy = media.0.join("Captures/My Captures");
    std::fs::create_dir_all(&legacy).unwrap();
    let mut old = format_zero_capture();
    cipher().apply(&mut old);
    std::fs::write(legacy.join("Old.cns"), old).unwrap();
    let log = Rc::new(RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher());
    let files = sim_files(&mut sim, &[]);
    let mut device = tetrad_app::device::Device::start(sim).unwrap();

    let view = (device.state().library.as_ref())
        .and_then(|library| library.database.as_ref())
        .unwrap()
        .view();
    assert!(view.product("broken").unwrap().is_none());
    assert_eq!(view.product_ids(ProductType::Capture).unwrap().len(), 1);
    let quarantined = view.quarantined_captures().unwrap();
    assert_eq!(
        (quarantined.len(), quarantined[0].id.as_str()),
        (1, "broken")
    );
    assert_eq!(device.take_corrupt_captures(), ["broken"]);
    assert!(device.take_corrupt_captures().is_empty());
    assert!(files.get(CAPTURE_CORRUPTION_CHECKED).is_some());
}

/// Review r23 K1: with the user store flagged global (a restored backup whose re-key did not
/// run) and a boot without the ZenUI seed, the store holds a seed-only-keyed row. The store
/// translation cannot run, so the corruption check waits: nothing moves to the quarantine, no
/// name reaches the dialog, and the marker stays missing for the next boot.
#[test]
fn the_corruption_check_waits_for_an_untranslated_store() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, ProductType, StockDb, StockProduct,
    };
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Global)?;
        write.put_product(&StockProduct {
            id: "older".into(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(vec![1, 2, 3]),
            metadata: Some(b"seed-only keyed".to_vec()),
        })
    })
    .unwrap();
    drop(db);
    let log = Rc::new(RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher());
    let files = sim_files(&mut sim, &[CAPTURE_CONVERSION_FINISHED]);
    let mut device = tetrad_app::device::Device::start(sim).unwrap();

    assert_eq!(library_notices(&log, "no ZenUI seed"), 1);
    assert_eq!(library_notices(&log, "capture corruption check waits"), 1);
    let view = (device.state().library.as_ref())
        .and_then(|library| library.database.as_ref())
        .unwrap()
        .view();
    assert!(view.product("older").unwrap().is_some());
    assert!(view.quarantined_captures().unwrap().is_empty());
    assert!(device.take_corrupt_captures().is_empty());
    assert!(files.get(CAPTURE_CORRUPTION_CHECKED).is_none());
}

/// B13 (boot 761): a CorOS 4.1.0 unit never writes `nc_encryption`, and its user captures are
/// device-keyed. Boot leaves the unflagged store as it is (stock `0x6a6f78` translates only a
/// global store): the metadata bytes stay, the flag stays unset and the capture reads.
#[test]
fn boot_keeps_an_unflagged_device_keyed_capture_store() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, ProductType, StockDb, StockProduct,
    };
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let metadata = sealed(&cipher(), br#"{"name":"Mine","version":"1"}"#);
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| {
        write.put_product(&StockProduct {
            id: "mine".into(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(vec![1, 2, 3]),
            metadata: Some(metadata.clone()),
        })
    })
    .unwrap();
    drop(db);
    let log = Rc::new(RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher());
    sim_files(
        &mut sim,
        &[CAPTURE_CORRUPTION_CHECKED, CAPTURE_CONVERSION_FINISHED],
    );
    let device = tetrad_app::device::Device::start(sim).unwrap();

    let db = device.state().library.as_ref().unwrap().database.as_ref();
    let view = db.unwrap().view();
    assert_eq!(
        view.capture_encryption(CaptureStore::User).unwrap(),
        CaptureEncryption::Unset
    );
    let row = view.product("mine").unwrap().unwrap();
    assert_eq!(row.metadata, Some(metadata));
    // The old rule took the store as seed-only-keyed and asked for the seed (none in the sim).
    assert_eq!(library_notices(&log, "no ZenUI seed"), 0);
    assert_eq!(library_notices(&log, "re-encrypted"), 0);
    assert_eq!(library_notices(&log, "capture metadata"), 0);
}

/// Review r28 A1: DOWNLOADS lists the products stored on the unit with no cloud listing this
/// run (`0x7ea828` reads `downloads_product_nc`): a capture's name and version come from its
/// device-ciphered metadata and its NC hash from the row, a preset's name and author from
/// its JSON metadata; each row is marked stored, so a tap loads it. The views read the same
/// rows on a worker thread (A3: the QC boot pass took 427 ms in that read): the first pass
/// builds them without the stored rows, and a later pass shows them.
#[test]
fn downloads_lists_the_stored_products_without_a_cloud_listing() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_app::device::services::DownloadsRow;
    use tetrad_services::cloud::services::ProductKind;
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, Download, DownloadKind, StockDb,
    };
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    let mut metadata = br#"{"name":"Plexi","version":"2"}"#.to_vec();
    cipher().apply(&mut metadata);
    let capture = Download {
        id: "c-1".into(),
        hash: Some("h-1".into()),
        raw_data: Some(vec![7; 8]),
        metadata: Some(metadata),
    };
    let preset = Download {
        id: "p-1".into(),
        hash: None,
        raw_data: Some(b"\x12\x04Lead".to_vec()),
        metadata: Some(br#"{"name":"Lead","author":"ann"}"#.to_vec()),
    };
    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::Downloads, CaptureEncryption::Local)?;
        write.put_download(DownloadKind::Capture, &capture)?;
        write.put_download(DownloadKind::Preset, &preset)
    })
    .unwrap();
    drop(db);
    let log = Rc::new(RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher());
    sim_files(
        &mut sim,
        &[CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED],
    );
    let mut device = tetrad_app::device::Device::start(sim).unwrap();

    let row =
        |id: &str, name: &str, author: &str, version: &str, hash: Option<&str>| DownloadsRow {
            id: id.into(),
            name: name.into(),
            author: author.into(),
            version: version.into(),
            hash: hash.map(str::to_owned),
            stored: true,
        };
    assert_eq!(
        device.downloads_rows(ProductKind::NeuralCapture),
        [row("c-1", "Plexi", "", "2", Some("h-1"))]
    );
    assert_eq!(
        device.downloads_rows(ProductKind::Preset),
        [row("p-1", "Lead", "ann", "", None)]
    );
    assert!(
        device
            .downloads_rows(ProductKind::ImpulseResponse)
            .is_empty()
    );
    let shown = |device: &tetrad_app::device::Device<common::Sim>| -> Vec<String> {
        (device.state().views.cloud.iter())
            .filter(|folder| folder.scope == tetrad_ui::CloudScope::Downloads)
            .flat_map(|folder| folder.rows.iter().map(|row| row.name.clone()))
            .collect()
    };
    device.step();
    assert!(
        shown(&device).is_empty(),
        "the first pass does not wait for the read"
    );
    for _ in 0..500 {
        if !shown(&device).is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        device.step();
    }
    assert_eq!(shown(&device), ["Lead", "Plexi"]);
}

/// re-r31 §5 (`0x1dd61c` → `0x1dcf2c`): a cloud preset of a run goes into its empty slot
/// (`store_cloud_preset`). The device checks every target before the first request: an
/// occupied slot (stock asks first, `0x46485c(name, 1)`, not built) or a slot used twice
/// refuses the whole run, so no request and no write happen.
#[test]
fn a_cloud_preset_download_goes_into_its_empty_slot_only() {
    use tetrad_app::device::services::{CloudDownload, DownloadOrigin};
    use tetrad_services::cloud::products::{CloudProduct, ProductPayload};
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let (mut device, _log, media) = common::boot(media);
    let setlist = "My Presets";
    let slots = |media: &common::Media| {
        std::fs::read_to_string(media.0.join("Presets/My Presets/setlist")).unwrap()
    };
    let before = slots(&media);
    for (items, reason) in [
        (
            vec![("p-1".to_owned(), 5), ("p-2".to_owned(), 0)],
            "slot 0 is occupied",
        ),
        (
            vec![("p-1".to_owned(), 5), ("p-2".to_owned(), 5)],
            "a slot used twice",
        ),
    ] {
        let refused = device.download_presets(setlist, items, Duration::ZERO);
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.to_string().contains(reason)),
            "{refused:?}"
        );
    }
    assert_eq!(slots(&media), before);
    assert!(device.bridges().services.downloads_in_flight().is_empty());

    let product = serde_json::json!({"id": "p-1", "name": "Cloud Lead", "type": "preset"});
    device.store_cloud_download(CloudDownload {
        product: CloudProduct::from_product(&product).unwrap(),
        payload: ProductPayload::Preset(b"\x12\x04Lead".to_vec()),
        origin: DownloadOrigin::Slot {
            setlist: setlist.into(),
            slot: 5,
            replace: None,
        },
    });
    let lines: Vec<String> = slots(&media).lines().map(str::to_owned).collect();
    assert_eq!(lines[5], format!("{}/Cloud Lead.pb", common::SETLIST));
    assert_eq!(lines[0], format!("{}/Light.pb", common::SETLIST));
}

/// OVERWRITE of "Overwrite Existing Presets?" (`0x46485c(name, 1)` → `0x1d8a04` →
/// `0x742990`): an occupied slot is replaced write-first, only while it still holds the
/// preset the user agreed to overwrite, and never the loaded preset (O, the PASTE rule):
/// before the first request and again at the store.
#[test]
fn an_overwrite_download_replaces_its_slot_but_never_the_loaded_preset() {
    use tetrad_app::device::services::{CloudDownload, DownloadOrigin};
    use tetrad_services::cloud::products::{CloudProduct, ProductPayload};
    let specs = common::STOCK.specs();
    let media = common::Media::new([
        ("Light", common::light_preset(&specs, 0)),
        ("Crunch", common::light_preset(&specs, 0)),
    ]);
    let (mut device, _log, media) = common::boot(media);
    let setlist = "My Presets";
    let (light, crunch) = (
        format!("{}/Light.pb", common::SETLIST),
        format!("{}/Crunch.pb", common::SETLIST),
    );
    let loaded = (device.state().current.as_ref()).and_then(|current| current.path.clone());
    assert_eq!(loaded.as_deref(), Some(light.as_str()));
    let slots = |media: &common::Media| {
        std::fs::read_to_string(media.0.join("Presets/My Presets/setlist")).unwrap()
    };
    let before = slots(&media);
    let items = vec![("p-1".to_owned(), 1), ("p-2".to_owned(), 0)];
    let refused = device.overwrite_presets(setlist, items, Duration::ZERO);
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("loaded preset's slot")),
        "{refused:?}"
    );
    assert!(device.bridges().services.downloads_in_flight().is_empty());

    let product = serde_json::json!({"id": "p-1", "name": "Cloud Lead", "type": "preset"});
    let store =
        |device: &mut tetrad_app::device::Device<common::Sim>, slot: usize, replace: &str| {
            device.store_cloud_download(CloudDownload {
                product: CloudProduct::from_product(&product).unwrap(),
                payload: ProductPayload::Preset(b"\x12\x04Lead".to_vec()),
                origin: DownloadOrigin::Slot {
                    setlist: setlist.into(),
                    slot,
                    replace: Some(replace.to_owned()),
                },
            });
        };
    // The slot changed after the check, or now holds the loaded preset: nothing is written.
    store(&mut device, 1, &format!("{}/Gone.pb", common::SETLIST));
    store(&mut device, 0, &light);
    assert_eq!(slots(&media), before);
    assert!(media.0.join("Presets/My Presets/Crunch.pb").exists());

    store(&mut device, 1, &crunch);
    let lines: Vec<String> = slots(&media).lines().map(str::to_owned).collect();
    assert_eq!(lines[1], format!("{}/Cloud Lead.pb", common::SETLIST));
    assert_eq!(lines[0], light);
    assert!(!media.0.join("Presets/My Presets/Crunch.pb").exists());
    assert!(!media.0.join("Presets/My Presets/Crunch.json").exists());
}

/// r33 task 3 (`0x79992c` → `0x799134`): a cloud IR goes to the IR library store
/// (`import_cloud_ir`, tested in tetrad-library), never to a Downloads table. Without the device
/// media cipher (as in this sim) it stores nothing and says why.
#[test]
fn a_cloud_ir_download_goes_to_the_ir_library_store() {
    use tetrad_app::device::services::{CloudDownload, DownloadOrigin};
    use tetrad_services::cloud::products::{CloudProduct, ProductPayload};
    use tetrad_storage::stock::ProductType;
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let (mut device, log, _media) = common::boot(media);
    let product = serde_json::json!({"id": "i-1", "name": "Room", "type": "impulse_response"});
    let irs = |device: &tetrad_app::device::Device<common::Sim>| {
        let db = device.state().library.as_ref().unwrap().database.as_ref();
        db.unwrap().view().product_ids(ProductType::Ir).unwrap()
    };
    let before = irs(&device);
    device.store_cloud_download(CloudDownload {
        product: CloudProduct::from_product(&product).unwrap(),
        payload: ProductPayload::Raw(ir_wav(64)),
        origin: DownloadOrigin::Cloud,
    });
    assert_eq!(irs(&device), before);
    let refused = "cloud IR i-1 not stored: no device media cipher";
    assert_eq!(library_notices(&log, refused), 1);
    assert_eq!(library_notices(&log, "Downloads"), 0);
}

/// A float mono 48 kHz WAV with an 18-byte `fmt` chunk, the layout of the stock extractor
/// `0x69c384` (invented samples).
fn ir_wav(samples: usize) -> Vec<u8> {
    let data: Vec<u8> = (0..samples)
        .flat_map(|i| (i as f32 / samples as f32).to_le_bytes())
        .collect();
    let mut body = b"WAVEfmt ".to_vec();
    body.extend(18_u32.to_le_bytes());
    body.extend(3_u16.to_le_bytes());
    body.extend(1_u16.to_le_bytes());
    body.extend(48_000_u32.to_le_bytes());
    body.extend(192_000_u32.to_le_bytes());
    body.extend([4, 0, 32, 0, 0, 0]);
    body.extend(b"data");
    body.extend((data.len() as u32).to_le_bytes());
    body.extend(data);
    let mut file = b"RIFF".to_vec();
    file.extend((body.len() as u32).to_le_bytes());
    file.extend(body);
    file
}

/// r27-control MR 2: a stored cloud product reaches a connected Cortex Control as the
/// Downloads push `0x250634` (File CREATE, the Downloads folder holding the stored row).
#[test]
fn a_stored_download_is_pushed_to_cortex_control() {
    use tetrad_app::device::services::{CloudDownload, DownloadOrigin};
    use tetrad_control::codecs::{ControlMessage, MessageAction};
    use tetrad_services::cloud::products::{CloudProduct, ProductPayload};
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let (mut device, log, _media) = common::boot(media);
    common::connect_control(&mut device);
    let product = serde_json::json!({"id": "cloud-preset", "name": "Lead", "type": "preset"});
    let mark = log.borrow().requests.len();
    device.store_cloud_download(CloudDownload {
        product: CloudProduct::from_product(&product).unwrap(),
        payload: ProductPayload::Preset(b"\x12\x05first".to_vec()),
        origin: DownloadOrigin::Queue,
    });
    (0..20).for_each(|_| _ = device.step());
    let pushes: Vec<_> = (common::control_sent(&log.borrow(), mark).into_iter())
        .filter_map(|message| match message {
            ControlMessage::File(file) => Some(file),
            _ => None,
        })
        .collect();
    let [push] = &pushes[..] else {
        panic!("one Downloads push: {pushes:?}");
    };
    let folder = push.folder.as_ref().unwrap();
    assert_eq!(push.action, Some(MessageAction::Create.into()));
    assert_eq!(folder.is_downloads, Some(true));
    let keys: Vec<_> = folder
        .files
        .iter()
        .map(|item| item.key.as_deref())
        .collect();
    assert_eq!(keys, [Some("cloud-preset")]);
}

/// Review r27 R1: an NC editor DOWNLOAD ends its job on the store of its product, `Stored`
/// or `Failed` (a store failure of that product), and sends no download queue `DELETE` (the
/// product was never queued), so no Services notice follows the store. Review r28 A2: a
/// Directory CLOUD row download sends no queue `DELETE` either.
#[test]
fn an_nc_editor_download_ends_on_its_store_without_a_queue_delete() {
    use tetrad_app::device::services::{
        CaptureDownloadEnd, CloudDownload, DownloadOrigin, DownloadOutcome,
    };
    use tetrad_services::cloud::products::{CloudProduct, ProductPayload};
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let (mut device, log, _media) = common::boot(media);
    let preset = serde_json::json!({"id": "c-1", "name": "Amp", "type": "preset"});
    let capture = serde_json::json!({"id": "c-2", "name": "Cab", "type": "neural_capture"});
    let mark = log.borrow().notices.len();
    device.store_cloud_download(CloudDownload {
        product: CloudProduct::from_product(&preset).unwrap(),
        payload: ProductPayload::Preset(b"\x12\x03Amp".to_vec()),
        origin: DownloadOrigin::Editor("h-1".into()),
    });
    device.store_cloud_download(CloudDownload {
        product: CloudProduct::from_product(&capture).unwrap(),
        payload: ProductPayload::Capture {
            id: "h-2".into(),
            record: b"not a capture".to_vec(),
            metadata: b"{}".to_vec(),
        },
        origin: DownloadOrigin::Editor("h-2".into()),
    });
    let owned = serde_json::json!({"id": "c-3", "name": "Mine", "type": "preset"});
    device.store_cloud_download(CloudDownload {
        product: CloudProduct::from_product(&owned).unwrap(),
        payload: ProductPayload::Preset(b"\x12\x04Mine".to_vec()),
        origin: DownloadOrigin::Cloud,
    });
    // The UI bridge drains the ends at the next step (r28-ui), so read them before it.
    let end = |hash: &str, outcome| CaptureDownloadEnd {
        hash: hash.into(),
        outcome,
    };
    assert_eq!(
        device.bridges().services.capture_download_ends(),
        [
            end("h-1", DownloadOutcome::Stored),
            end("h-2", DownloadOutcome::Failed)
        ]
    );
    (0..20).for_each(|_| _ = device.step());
    assert!(device.bridges().services.capture_download_ends().is_empty());
    let services: Vec<_> = (log.borrow().notices[mark..].iter())
        .filter(|notice| notice.subsystem == tetrad_app::device::Subsystem::Services)
        .map(|notice| notice.detail.clone())
        .collect();
    assert!(services.is_empty(), "{services:?}");
}

/// r21-services MR 4: a checked cloud product reaches its Downloads table through
/// `store_download` (stock `REPLACE`: a second store of the same id replaces it), and a
/// bring-up run without `--persist` refuses the effect.
#[test]
fn a_checked_cloud_product_is_stored_into_downloads() {
    use tetrad_app::device::Effect;
    use tetrad_app::device::bringup::{RunOptions, Stage};
    use tetrad_app::device::services::{CloudDownload, DownloadOrigin};
    use tetrad_services::cloud::products::{CloudProduct, ProductPayload};
    use tetrad_storage::stock::DownloadKind;
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let (mut device, log, media) = common::boot(media);
    let product = serde_json::json!({"id": "cloud-preset", "name": "Lead", "type": "preset"});
    let download = |raw: &[u8]| CloudDownload {
        product: CloudProduct::from_product(&product).unwrap(),
        payload: ProductPayload::Preset([&[0x12, raw.len() as u8][..], raw].concat()),
        origin: DownloadOrigin::Queue,
    };
    // Review r29 B3, B10: each store moves the cloud counter, so the DOWNLOADS view rebuilds.
    let before = device.bridges().services.cloud_changes();
    device.store_cloud_download(download(b"first"));
    let first = device.bridges().services.cloud_changes();
    device.store_cloud_download(download(b"second"));
    assert!(before < first && first < device.bridges().services.cloud_changes());
    assert_eq!(library_notices(&log, "cloud-preset added to Downloads"), 1);
    assert_eq!(
        library_notices(&log, "cloud-preset updated in Downloads"),
        1
    );
    let db = device.state().library.as_ref().unwrap().database.as_ref();
    let stored = db
        .unwrap()
        .view()
        .download(DownloadKind::Preset, "cloud-preset");
    assert_eq!(
        stored.unwrap().unwrap().raw_data.as_deref(),
        Some(&b"\x12\x06second"[..])
    );

    let bringup = RunOptions::bringup(
        Stage::Full,
        std::time::Duration::from_secs(60),
        media.0.join("logs"),
    );
    let effect = Effect::StoreDownload(Box::new(download(b"x")));
    assert!(bringup.refusal(&effect).is_some());
}

/// Protobuf writer for synthetic capture records (made-up values only).
#[derive(Default)]
struct Proto(Vec<u8>);

impl Proto {
    fn varint(mut self, field: u32, value: u64) -> Self {
        self.key(field, 0);
        self.raw(value);
        self
    }
    fn fixed(mut self, field: u32, bits: u32) -> Self {
        self.key(field, 5);
        self.0.extend(bits.to_le_bytes());
        self
    }
    fn bytes(mut self, field: u32, payload: &[u8]) -> Self {
        self.key(field, 2);
        self.raw(payload.len() as u64);
        self.0.extend(payload);
        self
    }
    fn floats(self, field: u32, values: &[f32]) -> Self {
        let packed: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.bytes(field, &packed)
    }
    fn key(&mut self, field: u32, wire: u64) {
        self.raw(u64::from(field) << 3 | wire);
    }
    fn raw(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.0.push(value as u8 | 0x80);
            value >>= 7;
        }
        self.0.push(value as u8);
    }
}

/// `captureProto.Capture` with the stock 1→2→2→1 shape (invented weights).
fn v1_capture() -> Vec<u8> {
    let mut sizes = Proto::default();
    [1u64, 2, 2, 1].into_iter().for_each(|size| sizes.raw(size));
    let layer =
        |weights: &[f32], biases: &[f32]| Proto::default().floats(1, weights).floats(2, biases).0;
    Proto::default()
        .fixed(1, 48_000.0f32.to_bits())
        .varint(2, 512)
        .varint(3, 2048)
        .floats(4, &[0.25; 512])
        .floats(5, &[0.5; 2048])
        .fixed(6, 1.0f32.to_bits())
        .fixed(7, 1.0f32.to_bits())
        .varint(8, 3)
        .bytes(9, &sizes.0)
        .bytes(10, &layer(&[1.0, 2.0], &[0.0, 0.0]))
        .bytes(10, &layer(&[1.0, 0.0, 0.0, 1.0], &[0.0, 0.0]))
        .bytes(10, &layer(&[0.5, 0.5], &[0.0]))
        .fixed(11, 20.0f32.to_bits())
        .fixed(12, 0.7f32.to_bits())
        .0
}

fn sealed(cipher: &DeviceCipher, plain: &[u8]) -> Vec<u8> {
    let mut bytes = plain.to_vec();
    cipher.apply(&mut bytes);
    bytes
}

/// A booted unit with both capture ciphers (made-up secrets), a local user capture store
/// that holds `kept` ("Kept", tags `a`), and a user folder "Gig" set as the NC saving
/// destination. Returns the device, its log, the media, the Gig folder id and the seed-only
/// cipher.
fn capture_unit(
    kept: &str,
) -> (
    tetrad_app::device::Device<common::Sim>,
    std::rc::Rc<std::cell::RefCell<common::Log>>,
    common::Media,
    i64,
    DeviceCipher,
) {
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_app::device::{Action, Event};
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, FolderType, NewFolder, ProductType, StockDb,
        StockProduct,
    };
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    let metadata = serde_json::json!({"name": "Kept", "tags": ["a"], "hash": kept});
    let gig = db
        .transaction(|write| {
            write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
            let root = write.boot_maintenance()?.0;
            write.put_product(&StockProduct {
                id: kept.into(),
                product_type: ProductType::Capture,
                read_only: false,
                raw_data: Some(b"kept model".to_vec()),
                metadata: Some(sealed(&cipher(), metadata.to_string().as_bytes())),
            })?;
            write.link(root, kept)?;
            write.create_folder(&NewFolder {
                product_type: ProductType::Capture,
                folder_type: FolderType::User,
                name: "Gig",
                parent: root,
                read_only: false,
            })
        })
        .unwrap();
    drop(db);
    let log = std::rc::Rc::new(std::cell::RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher());
    sim_files(
        &mut sim,
        &[CAPTURE_CORRUPTION_CHECKED, CAPTURE_CONVERSION_FINISHED],
    );
    let mut device = tetrad_app::device::Device::start(sim).unwrap();
    let seed = DeviceCipher::caller_secret(b"invented seed-only secret");
    device.set_capture_seed(seed.clone());
    device.push(Event::Action(Action::SetSavingDestination { folder: gig }));
    (0..5).for_each(|_| _ = device.step());
    (device, log, media, gig, seed)
}

/// A V1 cloud capture download with this hash and name, the record in `seed`.
fn cloud_capture(
    seed: &DeviceCipher,
    hash: &str,
    name: &str,
    origin: tetrad_app::device::services::DownloadOrigin,
) -> tetrad_app::device::services::CloudDownload {
    use tetrad_services::cloud::products::{CloudProduct, ProductPayload};
    let product = serde_json::json!({
        "id": format!("cloud-{name}"), "name": name, "type": "neural_capture", "tags": ["b"],
    });
    tetrad_app::device::services::CloudDownload {
        product: CloudProduct::from_product(&product).unwrap(),
        payload: ProductPayload::Capture {
            id: hash.into(),
            record: sealed(seed, &v1_capture()),
            metadata: b"{}".to_vec(),
        },
        origin,
    }
}

/// re-r34 §2 (`0x1d9954(.., 0x76be70(root, 2), ..)` → `0x6f8584` → `0x1e2bf4`): a Directory
/// CLOUD capture goes into the NC saving destination, not Downloads. A new hash lands there
/// [3]. A hash the store holds under another name [5/8] waits for the question and writes
/// nothing; OVERWRITE (`0x82ddfc`) then replaces only that capture's metadata (name,
/// `cloudUUID`), never its rawdata.
#[test]
fn a_cloud_capture_download_asks_before_it_renames_a_user_capture() {
    use tetrad_app::device::services::DownloadOrigin;
    use tetrad_storage::stock::DownloadKind;
    let kept = format!("{:0>64}", "c1");
    let fresh = format!("{:0>64}", "c2");
    let (mut device, log, _media, gig, seed) = capture_unit(&kept);
    let view = |device: &tetrad_app::device::Device<common::Sim>, id: &str| {
        let db = device.state().library.as_ref().unwrap().database.as_ref();
        db.unwrap().view().product(id).unwrap()
    };
    let before = view(&device, &kept).unwrap();

    device.store_cloud_download(cloud_capture(&seed, &fresh, "Fresh", DownloadOrigin::Cloud));
    let db = device.state().library.as_ref().unwrap().database.as_ref();
    let gig_items = db.unwrap().view().folder_products(gig).unwrap();
    assert_eq!(
        gig_items,
        [fresh.as_str()],
        "[3] lands in the saving destination"
    );
    assert!(
        db.unwrap()
            .view()
            .download_ids(DownloadKind::Capture)
            .unwrap()
            .is_empty()
    );

    device.store_cloud_download(cloud_capture(
        &seed,
        &kept,
        "Renamed",
        DownloadOrigin::Cloud,
    ));
    let question = device.bridges().services.capture_question().cloned();
    assert_eq!(
        question,
        Some(tetrad_ui::CaptureQuestion {
            name: "Renamed".into(),
            existing: "Kept".into()
        })
    );
    assert_eq!(
        view(&device, &kept).unwrap(),
        before,
        "nothing before OVERWRITE"
    );

    let overwrite = cloud_capture(&seed, &kept, "Renamed", DownloadOrigin::CaptureOverwrite);
    device.store_cloud_download(overwrite);
    let after = view(&device, &kept).unwrap();
    assert_eq!(after.raw_data, before.raw_data, "the rawdata stays");
    let mut metadata = after.metadata.unwrap();
    cipher().apply(&mut metadata);
    let metadata: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
    assert_eq!(metadata["name"], "Renamed");
    assert_eq!(metadata["cloudUUID"], "cloud-Renamed");
    let db = device.state().library.as_ref().unwrap().database.as_ref();
    assert_eq!(
        db.unwrap().view().folder_products(gig).unwrap(),
        [fresh.as_str()]
    );

    // Census K2: a new hash under a user capture's name is not stored; a single download
    // gets the single `0x464e30(name, 1)` text.
    let other = format!("{:0>64}", "c3");
    device.store_cloud_download(cloud_capture(&seed, &other, "Fresh", DownloadOrigin::Cloud));
    assert_eq!(view(&device, &other), None);
    let text = "A Neural Capture named Fresh already exists on your Quad Cortex.";
    assert_eq!(library_notices(&log, text), 1);
}

/// re-r34 §1 steps 4–6: a user-library capture upload is built from its row: `<hash>.cns`
/// re-keyed from the device cipher to the seed-only cipher (`0x6ae59c`), `<hash>.json` plain
/// (`0x7ee62c(record, 0)`), the meta-signature over it under the seed-only cipher
/// (`0x6fffe4`), the record name and tags. A Downloads capture is refused before any request
/// (re-r38 §1, O: stock has no defined single upload of one); a refused user capture is named
/// by its record. A good reply writes only the row's `cloudUUID` (`0x82ddfc`); a key the
/// store lacks writes nothing.
#[test]
fn a_user_capture_upload_is_built_from_its_row_and_writes_only_cloud_uuid() {
    use tetrad_app::device::services::{DownloadOrigin, capture_meta_signature};
    use tetrad_services::cloud::products::ProductPayload;
    use tetrad_services::cloud::services::ProductKind;
    let fresh = format!("{:0>64}", "c2");
    let (mut device, _log, media, _gig, seed) = capture_unit(&format!("{:0>64}", "c1"));
    device.store_cloud_download(cloud_capture(&seed, &fresh, "Fresh", DownloadOrigin::Cloud));
    let row = |device: &tetrad_app::device::Device<common::Sim>, id: &str| {
        let db = device.state().library.as_ref().unwrap().database.as_ref();
        db.unwrap().view().product(id).unwrap()
    };
    let plain = |row: tetrad_storage::stock::StockProduct| -> serde_json::Value {
        serde_json::from_slice(&sealed(&cipher(), &row.metadata.unwrap())).unwrap()
    };

    let draft = device.prepare_capture_upload(&fresh).unwrap();
    let upload = &draft.upload;
    assert_eq!(
        (draft.hash.as_str(), upload.kind),
        (fresh.as_str(), ProductKind::NeuralCapture)
    );
    assert_eq!(
        (upload.name.as_str(), &upload.tags),
        ("Fresh", &vec!["b".to_owned()])
    );
    assert_eq!(upload.cloud_id, None);
    let payload = ProductPayload::unpack(ProductKind::NeuralCapture, upload.payload.clone());
    let Ok(ProductPayload::Capture {
        id,
        record,
        metadata,
    }) = payload
    else {
        panic!("{payload:?}");
    };
    assert_eq!(id, fresh);
    assert_eq!(record, sealed(&seed, &v1_capture()), "the seed-only cipher");
    let fields: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
    assert_eq!(fields["name"], "Fresh", "plain JSON");
    assert_eq!(
        upload.meta_signature,
        capture_meta_signature(&seed, &metadata)
    );

    let insert = "INSERT INTO downloads_product_nc (id, hash, rawdata, metadata) \
                  VALUES ('cloud-5', ?1, x'00', x'00')";
    let downloaded = format!("{:0>64}", "d3");
    let connection = rusqlite::Connection::open(media.0.join("zenui.db")).unwrap();
    connection.execute(insert, [&downloaded]).unwrap();
    let refused = device.prepare_capture_upload(&downloaded).unwrap_err();
    assert_eq!(
        refused.reason,
        "Neural Captures in Downloads are uploaded only with a preset that uses them."
    );
    // Review r36 H6: a refusal names the record when its row reads ("Kept": its rawdata
    // does not load).
    let refused = device
        .prepare_capture_upload(&format!("{:0>64}", "c1"))
        .unwrap_err();
    assert_eq!(refused.name, "Kept", "{}", refused.reason);

    let before = row(&device, &fresh).unwrap();
    device
        .capture_uploaded(&fresh, "Fresh".into(), "cloud-up".into())
        .unwrap();
    let after = row(&device, &fresh).unwrap();
    assert_eq!(after.raw_data, before.raw_data, "the rawdata stays");
    let mut expected = plain(before);
    expected["cloudUUID"] = "cloud-up".into();
    assert_eq!(plain(after), expected, "only cloudUUID changes");
    let absent = format!("{:0>64}", "e4");
    assert!(
        device
            .capture_uploaded(&absent, "Gone".into(), "x".into())
            .is_err()
    );
    assert!(row(&device, &absent).is_none());
}

/// r37-services, the preset walk (`0x7414c0`, from the last capture block): each user capture
/// of the preset rides in the draft as its own upload (`Device::prepare_capture_upload`), in
/// walk order, for the cloud check; a factory capture stays a `dependencies.factory` hash.
/// Review r37 J3 (re-r38 §1, `0x6f6484`: user, then Downloads): a Downloads capture rides in
/// the walk too, built from its row the same way (`0x7bd384` re-key), marked as Downloads.
#[test]
fn a_preset_upload_carries_its_user_captures_for_the_walk() {
    use tetrad_app::device::services::{CaptureOrigin, DownloadOrigin};
    use tetrad_preset::{Model, ParamValue};
    use tetrad_services::cloud::products::ProductPayload;
    use tetrad_services::cloud::services::ProductKind;
    let [first, second, factory, downloaded] =
        ["c2", "c3", "f1", "d4"].map(|tail| format!("{tail:0>64}"));
    let (mut device, _log, media, _gig, seed) = capture_unit(&format!("{:0>64}", "c1"));
    for (hash, name) in [(&first, "First"), (&second, "Second")] {
        device.store_cloud_download(cloud_capture(&seed, hash, name, DownloadOrigin::Cloud));
    }
    let connection = |db: &str| rusqlite::Connection::open(media.0.join(db)).unwrap();
    let insert = "INSERT INTO product (id, type, readonly, rawdata, metadata) \
                  VALUES (?1, 2, 0, x'00', x'00')";
    connection("factory/factory.db")
        .execute(insert, [&factory])
        .unwrap();
    let metadata = serde_json::json!({"name": "Fetched NC", "tags": ["c"], "authorId": ""});
    let insert = "INSERT INTO downloads_product_nc (id, hash, rawdata, metadata) \
                  VALUES ('cloud-5', ?1, ?2, ?3)";
    let row = (
        &downloaded,
        sealed(&cipher(), &v1_capture()),
        sealed(&cipher(), metadata.to_string().as_bytes()),
    );
    let user_db = connection("zenui.db");
    user_db.execute(insert, row).unwrap();
    let local = "INSERT OR REPLACE INTO nc_encryption (nc_type, encryption) VALUES (?1, 1)";
    user_db.execute(local, ["downloads"]).unwrap();
    let factory_list = media.0.join("factory/Factory Library");
    std::fs::create_dir_all(&factory_list).unwrap();
    let empty = tetrad_storage::media::encode_setlist(&vec![None; 256]).unwrap();
    std::fs::write(factory_list.join("setlist"), empty).unwrap();
    let store = |name: &str, keys: &[&str]| {
        let mut preset = common::light_preset(&common::STOCK.specs(), 0);
        for key in keys {
            let mut values = vec![ParamValue::Float(0.0); 5];
            values.push(ParamValue::String(format!("{key}NC")));
            preset.chains[0].models.push(Model::block(14_000, values));
        }
        let file = media.0.join(format!("Presets/My Presets/{name}"));
        std::fs::write(file.with_extension("pb"), preset.to_bytes()).unwrap();
        let sidecar = serde_json::json!({"name": name, "date": "2026-10-12T10:00:00Z"});
        std::fs::write(file.with_extension("json"), sidecar.to_string()).unwrap();
        format!("{}/{name}.pb", common::SETLIST)
    };
    let walked = store("Walked", &[&first, &factory, &second]);
    let fetched = store("Fetched", &[&first, &downloaded]);

    let draft = device.prepare_upload(&walked).unwrap();
    let hashes: Vec<&str> = (draft.captures.iter())
        .map(|c| c.check.hash.as_str())
        .collect();
    assert_eq!(hashes, [second.as_str(), first.as_str()], "walk order");
    assert_eq!(draft.upload.factory_dependencies, [factory]);
    let own = device.prepare_capture_upload(&second).unwrap();
    let walked = draft.captures[0].upload.as_ref().unwrap();
    assert_eq!(
        (&walked.name, &walked.payload),
        (&own.upload.name, &own.upload.payload)
    );
    assert_eq!(draft.captures[0].check.name, own.upload.name);
    let draft = device.prepare_upload(&fetched).unwrap();
    let walk: Vec<(&str, CaptureOrigin)> = (draft.captures.iter())
        .map(|capture| (capture.check.hash.as_str(), capture.origin))
        .collect();
    assert_eq!(
        walk,
        [
            (downloaded.as_str(), CaptureOrigin::Downloads),
            (first.as_str(), CaptureOrigin::User)
        ]
    );
    let upload = draft.captures[0].upload.as_ref().unwrap();
    assert_eq!(upload.name, "Fetched NC");
    let payload = ProductPayload::unpack(ProductKind::NeuralCapture, upload.payload.clone());
    let Ok(ProductPayload::Capture { record, .. }) = payload else {
        panic!("{payload:?}");
    };
    assert_eq!(record, sealed(&seed, &v1_capture()), "the seed-only cipher");
}

/// Review r39 K9 (J7): the walk builds a capture's own upload only for its upload (status 3
/// or OVERWRITE). Another author's Downloads capture whose body does not build (here an NC2
/// record that does not load) does not refuse the preset: the draft keeps its check fields
/// (`0x6f94ec`: name, tags, `authorId` from the record in the device cipher) and the reason.
#[test]
fn a_walk_capture_that_does_not_build_still_rides_for_its_check() {
    use tetrad_preset::{Model, ParamValue};
    let theirs = format!("{:0>64}", "d5");
    let (mut device, _log, media, _gig, _seed) = capture_unit(&format!("{:0>64}", "c1"));
    let metadata = serde_json::json!({"name": "Their NC", "tags": ["t"],
        "authorId": "someone-else", "version": "2"});
    let user_db = rusqlite::Connection::open(media.0.join("zenui.db")).unwrap();
    let insert = "INSERT INTO downloads_product_nc (id, hash, rawdata, metadata) \
                  VALUES ('cloud-6', ?1, ?2, ?3)";
    let row = (
        &theirs,
        sealed(&cipher(), b"not an NC2 file"),
        sealed(&cipher(), metadata.to_string().as_bytes()),
    );
    user_db.execute(insert, row).unwrap();
    let local = "INSERT OR REPLACE INTO nc_encryption (nc_type, encryption) VALUES (?1, 1)";
    user_db.execute(local, ["downloads"]).unwrap();
    let factory_list = media.0.join("factory/Factory Library");
    std::fs::create_dir_all(&factory_list).unwrap();
    let empty = tetrad_storage::media::encode_setlist(&vec![None; 256]).unwrap();
    std::fs::write(factory_list.join("setlist"), empty).unwrap();
    let mut preset = common::light_preset(&common::STOCK.specs(), 0);
    let mut values = vec![ParamValue::Float(0.0); 5];
    values.push(ParamValue::String(format!("{theirs}NC")));
    preset.chains[0].models.push(Model::block(14_000, values));
    let file = media.0.join("Presets/My Presets/Borrowed");
    std::fs::write(file.with_extension("pb"), preset.to_bytes()).unwrap();
    let sidecar = serde_json::json!({"name": "Borrowed", "date": "2026-10-12T10:00:00Z"});
    std::fs::write(file.with_extension("json"), sidecar.to_string()).unwrap();
    let path = format!("{}/Borrowed.pb", common::SETLIST);

    let draft = device.prepare_upload(&path);
    let draft = draft.unwrap_or_else(|refused| panic!("not refused: {refused:?}"));
    let [walked] = &draft.captures[..] else {
        panic!("{:?}", draft.captures);
    };
    assert_eq!(
        (walked.check.name.as_str(), walked.check.author_id.as_str()),
        ("Their NC", "someone-else")
    );
    assert_eq!(walked.check.tags, ["t"]);
    assert!(
        (walked.upload.as_ref()).is_err_and(|reason| reason.contains("does not load")),
        "{:?}",
        walked.upload
    );
}

/// Review r33 F1 (O): a capture dependency whose hash is not the key its preset names is not
/// stored and is not reported as added; one with the right hash goes on to the store. F3 (C,
/// `0x1d8e9c` → `0x1d8bac`): a factory hash is added to the user store like any other.
#[test]
fn a_capture_dependency_with_another_hash_is_refused() {
    use tetrad_app::device::services::DownloadOrigin;
    let named = format!("{:0>64}", "d1");
    let other = format!("{:0>64}", "d2");
    let (mut device, log, media, _gig, seed) = capture_unit(&format!("{:0>64}", "c1"));
    let dependency = |hash: &str| DownloadOrigin::Dependency {
        preset: "p-1".into(),
        hash: hash.into(),
    };
    device.store_cloud_download(cloud_capture(&seed, &other, "Cab", dependency(&named)));
    let refused = format!("its capture hash {other} is not the hash {named} the preset names");
    assert_eq!(library_notices(&log, &refused), 1);
    assert_eq!(library_notices(&log, "added to the Captures Library"), 0);
    let db = device.state().library.as_ref().unwrap().database.as_ref();
    assert!(db.unwrap().view().product(&other).unwrap().is_none());

    let factory = rusqlite::Connection::open(media.0.join("factory/factory.db")).unwrap();
    let insert = "INSERT INTO product (id, type, readonly, rawdata, metadata) \
                  VALUES (?1, 2, 1, x'00', x'00')";
    factory.execute(insert, [&named]).unwrap();
    device.store_cloud_download(cloud_capture(&seed, &named, "Cab", dependency(&named)));
    assert_eq!(library_notices(&log, "added to the Captures Library"), 1);
    let db = device.state().library.as_ref().unwrap().database.as_ref();
    assert!(db.unwrap().view().product(&named).unwrap().is_some());
}

/// re-r34 §4: a queue preset's capture goes into the captures Downloads table, also when the
/// user store holds its hash (no `0x6f8584` check, the user row stays); one whose hash is not
/// the key its preset names is not stored (review r33 F1, O).
#[test]
fn a_queue_preset_capture_goes_into_downloads() {
    use tetrad_app::device::services::DownloadOrigin;
    use tetrad_storage::stock::DownloadKind;
    let kept = format!("{:0>64}", "c1");
    let (mut device, _log, _media, _gig, seed) = capture_unit(&kept);
    let user_row = |device: &tetrad_app::device::Device<common::Sim>| {
        let db = device.state().library.as_ref().unwrap().database.as_ref();
        db.unwrap().view().product(&kept).unwrap()
    };
    let downloads = |device: &tetrad_app::device::Device<common::Sim>| {
        let db = device.state().library.as_ref().unwrap().database.as_ref();
        db.unwrap()
            .view()
            .download_ids(DownloadKind::Capture)
            .unwrap()
    };
    let before = user_row(&device);
    let queue = |hash: &str| DownloadOrigin::QueueDependency {
        preset: "p-1".into(),
        hash: hash.into(),
    };
    let other = format!("{:0>64}", "d9");
    device.store_cloud_download(cloud_capture(&seed, &other, "Wrong", queue(&kept)));
    assert!(downloads(&device).is_empty());
    device.store_cloud_download(cloud_capture(&seed, &kept, "Renamed", queue(&kept)));
    assert_eq!(downloads(&device), ["cloud-Renamed"]);
    assert_eq!(user_row(&device), before);
}

/// r35-library MR 2: a Directory move onto an occupied slot of the same setlist is a stock swap
/// and pushes the swap undo frame (type 2, `0x648a24`: what each slot names after the swap); a
/// move onto an empty slot pushes none. A setlist rename (O) and delete (`0x647c84`) drop the
/// swap frames that name it.
#[test]
fn a_directory_swap_pushes_a_swap_frame_and_setlist_edits_drop_it() {
    use tetrad_ui::DirectoryRequest as R;
    let mut fixture = DirectoryFixture::new();
    let preset =
        tetrad_preset::PresetDocument::parse(&[], tetrad_preset::Limits::default()).unwrap();
    let library = fixture.state.library.as_mut().unwrap();
    library.presets.create_setlist("Live").unwrap();
    let mut store = |slot: usize, name: &str| {
        let sidecar = tetrad_library::sidecar::Sidecar::new_preset(
            name,
            std::time::SystemTime::UNIX_EPOCH,
            "4.1.0",
        );
        (library.presets.store("Live", slot, name, &preset, &sidecar))
            .unwrap()
            .value
    };
    let (a, b) = (store(0, "A"), store(1, "B"));
    let swap = |setlist: &str, from, to| R::MovePreset {
        setlist: setlist.into(),
        from,
        to,
        overwrite: false,
    };
    fixture.request(swap("Live", 0, 1));
    assert_eq!(fixture.state.history.frame_types().0, [2]);
    let sides = fixture.state.history.newest_swap(false).unwrap();
    assert_eq!(
        sides.clone().map(|side| (side.slot, side.path)),
        [(0, Some(b)), (1, Some(a))]
    );
    fixture.request(swap("Live", 1, 5));
    assert_eq!(
        fixture.state.history.frame_types().0,
        [2],
        "a move to an empty slot"
    );

    fixture.request(R::RenameSetlist {
        from: "Live".into(),
        to: "Tour".into(),
    });
    assert!(fixture.state.history.frame_types().0.is_empty());
    fixture.request(swap("Tour", 0, 5));
    assert_eq!(fixture.state.history.frame_types().0, [2]);
    fixture.request(R::DeleteSetlist {
        name: "Tour".into(),
    });
    assert!(fixture.state.history.frame_types().0.is_empty());
}
