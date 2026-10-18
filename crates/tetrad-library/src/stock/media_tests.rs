//! Media library rules of r16. Synthetic files, databases and secrets only.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use tetrad_media::factory::DeviceCipher;
use tetrad_media::ir::IrLimits;
use tetrad_preset::{
    CatalogModel, CatalogParam, Model, ModelCatalog, ParamValue, Preset, Slot, SplitterType,
};
use tetrad_storage::media::{MediaRoot, SETLIST_SLOTS, encode_setlist};
use tetrad_storage::stock::{DbLimits, ProductType, StockDb, StockProduct};

use super::media_query::{CaptureFacts, CaptureFilter, DateJumpBar, JumpTargets, letter_label};
use super::*;
use crate::Error;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A synthetic `/media/p4` with "My Presets" and a new `zenui.db`.
struct Media(PathBuf);

impl Media {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-library-media-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("Presets/My Presets")).unwrap();
        fs::write(
            root.join("Presets/My Presets/setlist"),
            encode_setlist(&vec![None; SETLIST_SLOTS]).unwrap(),
        )
        .unwrap();
        Self(root)
    }

    fn library(&self) -> StockLibrary {
        StockLibrary::open(MediaRoot::new(&self.0), StockLimits::default()).unwrap()
    }

    fn database(&self) -> StockDb {
        let mut db = StockDb::create(&self.0.join("zenui.db"), DbLimits::default()).unwrap();
        db.transaction(|write| write.boot_maintenance()).unwrap();
        db
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn cipher() -> DeviceCipher {
    DeviceCipher::caller_secret(b"synthetic device cipher")
}

fn root(db: &StockDb, kind: ProductType) -> i64 {
    db.view().root_folder(kind).unwrap().unwrap()
}

/// An 18-byte-fmt float WAV, the layout of the stock extractor.
fn wav(channels: u16, samples: usize) -> Vec<u8> {
    let data: Vec<u8> = (0..samples)
        .flat_map(|i| (i as f32 / samples as f32).to_le_bytes())
        .collect();
    let mut body = b"WAVEfmt ".to_vec();
    body.extend_from_slice(&18_u32.to_le_bytes());
    body.extend_from_slice(&3_u16.to_le_bytes());
    body.extend_from_slice(&channels.to_le_bytes());
    body.extend_from_slice(&48_000_u32.to_le_bytes());
    body.extend_from_slice(&192_000_u32.to_le_bytes());
    body.extend_from_slice(&[4, 0, 32, 0, 0, 0]);
    body.extend_from_slice(b"data");
    body.extend_from_slice(&(data.len() as u32).to_le_bytes());
    body.extend_from_slice(&data);
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    file
}

const LIMITS: IrLimits = IrLimits {
    max_asset_bytes: 1 << 20,
    max_chunks: 16,
};

/// `0x0024dae0`: mono 48 kHz float only, the first 1,024 samples, id by
/// MD5, linked to the root and the target folder; the same WAV replaces.
#[test]
fn ir_import_follows_the_stock_extractor_and_replaces_by_id() {
    let media = Media::new();
    let mut db = media.database();
    let top = root(&db, ProductType::Ir);
    let folder = create_media_folder(&mut db, ProductType::Ir, top, "Cabs").unwrap();
    fn import(wav: &[u8]) -> IrImport<'_> {
        IrImport {
            wav,
            name: "Long IR",
            date: UNIX_EPOCH + Duration::from_secs(1_791_635_696),
            cloud_id: "",
            coros_version: "4.1.0",
        }
    }
    let long = wav(1, 1500);
    let stored = import_user_ir(&mut db, folder, &import(&long), &cipher(), LIMITS).unwrap();
    assert!(!stored.replaced);
    let row = db.view().product(&stored.id).unwrap().unwrap();
    assert_eq!(row.raw_data.unwrap().len(), 1024 * 4);
    for linked in [top, folder] {
        assert_eq!(
            db.view().folder_products(linked).unwrap(),
            std::slice::from_ref(&stored.id)
        );
    }
    let again = import_user_ir(&mut db, folder, &import(&long), &cipher(), LIMITS).unwrap();
    assert!(again.replaced);
    for refused in [wav(2, 64), Vec::new()] {
        let error = import_user_ir(&mut db, folder, &import(&refused), &cipher(), LIMITS);
        assert!(matches!(error, Err(Error::Media(_))), "{error:?}");
    }
}

/// `0x79992c` → `0x1de9f8` → `0x82e91c`: a cloud IR is stored with the `0x7ee62c` case 1
/// fields of its cloud record (a null date is ""). An IR with the same samples or a user IR
/// with the same name is not written (O: stock asks in dialogs not traced).
#[test]
fn a_cloud_ir_keeps_its_cloud_record_and_never_replaces_a_user_ir() {
    use tetrad_media::user::UserIrMetadata;
    use tetrad_services::cloud::products::CloudProduct;
    let media = Media::new();
    let mut db = media.database();
    let top = root(&db, ProductType::Ir);
    let cloud = |id: &str, name: &str, created: Option<&str>| {
        let mut reply = serde_json::json!({
            "id": id, "name": name, "type": "impulse_response", "creator_version": "4.1.0",
        });
        if let Some(created) = created {
            reply["created"] = created.into();
        }
        CloudProduct::from_product(&reply).unwrap()
    };
    let room = cloud("i-1", "Room", Some("1970-01-02T00:00:01Z"));
    let added = import_cloud_ir(&mut db, top, &wav(1, 1500), &room, &cipher(), LIMITS).unwrap();
    let CloudIr::Added(id) = added else {
        panic!("{added:?}");
    };
    let row = db.view().product(&id).unwrap().unwrap();
    assert_eq!(row.raw_data.as_ref().map(Vec::len), Some(1024 * 4));
    let metadata = UserIrMetadata::from_blob(row.metadata.as_ref().unwrap(), &cipher()).unwrap();
    let expected = UserIrMetadata {
        name: "Room".into(),
        date: "1970-01-02T00:00:01Z".into(),
        cloud_id: "i-1".into(),
        coros_version: "4.1.0".into(),
    };
    assert_eq!(metadata, expected);

    let same = cloud("i-2", "Other", None);
    let present = import_cloud_ir(&mut db, top, &wav(1, 1500), &same, &cipher(), LIMITS);
    let name = "Room".to_owned();
    assert_eq!(
        present.unwrap(),
        CloudIr::Present {
            id: id.clone(),
            name
        }
    );
    let taken = import_cloud_ir(&mut db, top, &wav(1, 900), &room, &cipher(), LIMITS);
    assert_eq!(taken.unwrap(), CloudIr::NameTaken);
    assert_eq!(
        db.view().product_ids(ProductType::Ir).unwrap(),
        std::slice::from_ref(&id)
    );
    let stored = db.view().product(&id).unwrap().unwrap();
    assert_eq!(
        stored.metadata, row.metadata,
        "the user IR is not rewritten"
    );

    let hall = cloud("i-3", "Hall", None);
    let added = import_cloud_ir(&mut db, top, &wav(1, 900), &hall, &cipher(), LIMITS).unwrap();
    let CloudIr::Added(hall_id) = added else {
        panic!("{added:?}");
    };
    let blob = db
        .view()
        .product(&hall_id)
        .unwrap()
        .unwrap()
        .metadata
        .unwrap();
    assert_eq!(
        UserIrMetadata::from_blob(&blob, &cipher()).unwrap().date,
        ""
    );
}

/// Synthetic catalog: every model has 24 float params; 23000/28000 are the
/// lane controls `Preset::new_default` needs.
struct Catalog;

impl ModelCatalog for Catalog {
    fn model(&self, _: u32) -> Option<CatalogModel> {
        Some(CatalogModel {
            params: (0..24)
                .map(|_| CatalogParam {
                    default: ParamValue::Float(0.0),
                    expression_assignable: true,
                })
                .collect(),
            sidechain_parameter: None,
        })
    }

    fn splitter_type(&self, _: SplitterType) -> Option<f32> {
        Some(0.0)
    }

    fn tempo_value(&self, _: u32) -> Option<f32> {
        Some(0.0)
    }

    fn is_complete(&self) -> bool {
        true
    }
}

/// `0x006e72d4` + `0x006e6138` + `0x006e45dc`.
#[test]
fn legacy_irs_move_into_the_database_presets_follow_and_the_directory_is_kept_aside() {
    const DIR: &str = "CustomIRs/Impulse Responses";
    let media = Media::new();
    let mut db = media.database();
    let legacy = media.0.join(DIR);
    fs::create_dir_all(&legacy).unwrap();
    let words = |n: usize| {
        (0..n)
            .flat_map(|i| (i as f32).to_le_bytes())
            .collect::<Vec<u8>>()
    };
    fs::write(legacy.join("Room.bin"), words(4)).unwrap();
    fs::write(
        legacy.join("Room.json"),
        br#"{"name": "My Room","date": "2020-02-02T00:00:00Z"}"#,
    )
    .unwrap();
    fs::write(legacy.join("Hall.bin"), words(8)).unwrap();
    fs::write(legacy.join("Odd.bin"), [0_u8; 6]).unwrap();
    let path = format!("/media/p4/{DIR}/Room.bin");
    let mut values = vec![ParamValue::Float(0.0); 24];
    values[2] = ParamValue::String(path.clone());
    values[22] = ParamValue::String("old name".into());
    let mut preset = Preset::new_default("4.1.0", &Catalog).unwrap();
    preset
        .insert_block(
            Slot { row: 0, column: 1 },
            Model::block(29001, values),
            &Catalog,
        )
        .unwrap();
    fs::write(
        media.0.join("Presets/My Presets/Loader.pb"),
        preset.to_bytes(),
    )
    .unwrap();

    let mut library = media.library();
    let model = |id: u32| (id / 1000 == 29).then_some((29, id));
    let report = library
        .migrate_legacy_irs(&mut db, &cipher(), "4.1.0", model)
        .unwrap()
        .unwrap();
    assert_eq!(report.skipped, ["Odd.bin"]);
    assert_eq!(report.presets, ["/media/p4/Presets/My Presets/Loader.pb"]);
    let room = tetrad_media::user::product_id(&words(4));
    assert!(report.products.contains(&room));
    let row = db.view().product(&room).unwrap().unwrap();
    let metadata =
        tetrad_media::user::UserIrMetadata::from_blob(&row.metadata.unwrap(), &cipher()).unwrap();
    assert_eq!(
        (metadata.name.as_str(), metadata.date.as_str()),
        ("My Room", "2020-02-02T00:00:00Z")
    );
    assert_eq!(
        db.view()
            .folder_products(root(&db, ProductType::Ir))
            .unwrap()
            .len(),
        2
    );

    let document = library
        .load("/media/p4/Presets/My Presets/Loader.pb")
        .unwrap()
        .document;
    let preset = Preset::try_from(&document).unwrap();
    let block = preset.block(Slot { row: 0, column: 1 }).unwrap().unwrap();
    for (param, expected) in [(2, room.as_str()), (22, "Room")] {
        assert_eq!(
            block.params[param].value(0),
            Some(&ParamValue::String(expected.into()))
        );
    }
    assert!(!legacy.exists());
    assert!(media.0.join(format!("{DIR}_legacy/Odd.bin")).exists());
    assert!(
        library
            .migrate_legacy_irs(&mut db, &cipher(), "4.1.0", model)
            .unwrap()
            .is_none()
    );
}

/// `0x006e6c18`: id = base name, bytes as stored; no metadata, no row.
#[test]
fn legacy_captures_without_metadata_are_skipped() {
    let media = Media::new();
    let mut db = media.database();
    let legacy = media.0.join("Captures/My Captures");
    fs::create_dir_all(&legacy).unwrap();
    let key = "ab".repeat(32);
    fs::write(legacy.join(format!("{key}.cns")), b"model").unwrap();
    fs::write(legacy.join(format!("{key}.json")), b"encrypted metadata").unwrap();
    fs::write(legacy.join("lonely.cns"), b"model").unwrap();
    let report = media
        .library()
        .migrate_legacy_captures(&mut db)
        .unwrap()
        .unwrap();
    assert_eq!(
        (report.products, report.skipped),
        (vec![key.clone()], vec!["lonely.cns".to_owned()])
    );
    let row = db.view().product(&key).unwrap().unwrap();
    assert_eq!(row.metadata.as_deref(), Some(&b"encrypted metadata"[..]));
    assert_eq!(
        db.view()
            .folder_products(root(&db, ProductType::Capture))
            .unwrap(),
        [key]
    );
    assert!(media.0.join("Captures/My Captures_legacy").is_dir());
}

/// `0x00766790`: 30 writable folders of a type, root included; parents
/// below depth 4.
#[test]
fn folder_creation_counts_the_root_and_limits_nesting() {
    let media = Media::new();
    let mut db = media.database();
    let mut parent = root(&db, ProductType::Ir);
    for depth in 0..4 {
        parent =
            create_media_folder(&mut db, ProductType::Ir, parent, &format!("L{depth}")).unwrap();
    }
    let deep = create_media_folder(&mut db, ProductType::Ir, parent, "L4");
    assert!(matches!(
        deep,
        Err(Error::Storage(tetrad_storage::Error::Limit))
    ));
    let top = root(&db, ProductType::Ir);
    for n in 5..30 {
        create_media_folder(&mut db, ProductType::Ir, top, &format!("F{n}")).unwrap();
    }
    let full = create_media_folder(&mut db, ProductType::Ir, top, "F30");
    assert!(matches!(
        full,
        Err(Error::Storage(tetrad_storage::Error::Limit))
    ));
}

/// `0x0076dabc`: the copy takes the next free sibling name, subfolders keep
/// theirs, links are copied; the root cannot be duplicated.
#[test]
fn duplicate_copies_the_subtree_and_links_under_a_free_name() {
    let media = Media::new();
    let mut db = media.database();
    let top = root(&db, ProductType::Ir);
    let rock = create_media_folder(&mut db, ProductType::Ir, top, "Rock").unwrap();
    create_media_folder(&mut db, ProductType::Ir, top, "Rock 2").unwrap();
    let child = create_media_folder(&mut db, ProductType::Ir, rock, "Live").unwrap();
    let product = StockProduct {
        id: "CIR_1".into(),
        product_type: ProductType::Ir,
        read_only: false,
        raw_data: Some(vec![0; 4]),
        metadata: None,
    };
    store_media(&mut db, &product, child).unwrap();
    let copy = duplicate_media_folder(&mut db, rock).unwrap();
    let folders = db.view().folders().unwrap();
    let named = |id| folders.iter().find(|f| f.id == id).unwrap();
    assert_eq!(named(copy).name, "Rock 3");
    let live = folders.iter().find(|f| f.parent == copy).unwrap();
    assert_eq!(live.name, "Live");
    assert_eq!(db.view().folder_products(live.id).unwrap(), ["CIR_1"]);
    assert!(duplicate_media_folder(&mut db, top).is_err());

    assert_eq!(next_folder_name("Rock"), "Rock 2");
    assert_eq!(next_folder_name("7"), "8");
    assert_eq!(next_folder_name("Very Long Folder 9"), "Very Long Fol 10");
}

/// Saving destination keys: `local_ir_root`, `<id>_q`; anything else is the root.
#[test]
fn saving_destination_resolves_stock_keys_and_falls_back_to_the_root() {
    let media = Media::new();
    let mut db = media.database();
    let top = root(&db, ProductType::Ir);
    let folder = create_media_folder(&mut db, ProductType::Ir, top, "Mine").unwrap();
    let view = db.view();
    let key = media_folder_key(
        view.folders()
            .unwrap()
            .iter()
            .find(|f| f.id == folder)
            .unwrap(),
    );
    assert_eq!(key.as_deref(), Some(format!("{folder}_q").as_str()));
    for (stored, expected) in [
        (key.as_deref(), folder),
        (Some(IR_ROOT_KEY), top),
        (Some("999_q"), top),
        (None, top),
    ] {
        assert_eq!(
            saving_destination(&view, ProductType::Ir, stored).unwrap(),
            expected
        );
    }
    let capture_root = root(&db, ProductType::Capture);
    let wrong_type = saving_destination(&view, ProductType::Capture, key.as_deref()).unwrap();
    assert_eq!(wrong_type, capture_root);
}

/// Filter groups, jump bar labels and targets, date bar (`0x008072b4`).
#[test]
fn capture_queries_filter_and_feed_the_jump_bars() {
    let facts = |id: &str, json: &str| CaptureFacts::from_metadata(id, json.as_bytes()).unwrap();
    let rows = [
        facts(
            "a",
            r#"{"name": "2 Rock","deviceType": "cab","gainType": 3,"authorId": "me","version": "2","date": "2021-01-11T10:00:00Z"}"#,
        ),
        facts(
            "b",
            r#"{"name": "amp","deviceType": "amp_head","gainType": -1,"authorId": "you","version": "1"}"#,
        ),
        facts(
            "c",
            r#"{"name": "Bass","deviceType": "cab","gainType": 7,"authorId": "me","version": "2","date": "2021-03-01T00:00:00Z"}"#,
        ),
    ];
    let cabs = [4];
    let filter = CaptureFilter {
        device_types: Some(&cabs),
        versions: Some(&["2"]),
        owned_by: Some("me"),
    };
    let kept: Vec<_> = rows
        .iter()
        .filter(|row| filter.matches(row))
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(kept, ["a", "c"]);
    assert!(rows.iter().all(|row| CaptureFilter::default().matches(row)));

    assert_eq!(
        [0, 8, 9, 34, 35].map(letter_label),
        [Some('#'), Some('#'), Some('A'), Some('Z'), None]
    );
    let targets = JumpTargets::new(&rows);
    assert_eq!(
        (targets.letters[0], targets.letters[1], targets.letters[2]),
        (Some(0), Some(1), Some(2))
    );
    assert_eq!((targets.devices[4], targets.devices[1]), (Some(0), Some(1)));
    assert_eq!(
        (targets.gains[3], targets.gains[7], targets.gains[0]),
        (Some(0), Some(2), None)
    );

    let bar = DateJumpBar::new(rows[0].date, rows[2].date, 0);
    assert_eq!(bar.date_at(0), rows[0].date);
    assert_eq!(bar.date_at(200), rows[2].date);
    assert_eq!(bar.date_at(100), Some(rows[0].date.unwrap() + 25));
    let defaulted = DateJumpBar::new(None, None, 18_628 + 10);
    assert_eq!(
        (defaulted.date_at(0), defaulted.date_at(200)),
        (None, Some(18_638))
    );
    assert_eq!(super::media_query::date_target(&rows, 18_628 + 60), Some(2));
}
