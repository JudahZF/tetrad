use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rusqlite::Connection;

use super::media::{
    MediaRoot, SETLIST_SLOTS, decode_lines, decode_setlist, encode_lines, encode_setlist,
    write_atomic,
};
use super::search_index::{SearchEntry, SearchIndex};
use super::stock::*;
use super::{Error, Library, Limits, Result};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "tetrad-storage-stock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    /// A synthetic stock-shaped database at the given schema version.
    fn database(&self, major: i64, minor: i64) -> PathBuf {
        let path = self.0.join("zenui.db");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(USER_SCHEMA).unwrap();
        connection
            .execute("INSERT INTO schema_version VALUES (?1, ?2)", [major, minor])
            .unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn open(path: &Path) -> StockDb {
    StockDb::open(path, DbLimits::default(), false).unwrap()
}

fn param(index: i64, value: &str) -> ParamEntry {
    ParamEntry {
        index,
        value: value.to_owned(),
        exp: 0,
        exp_trim_max: 1.0,
        exp_trim_min: 0.0,
    }
}

const SWITCH: ExpSwitch = ExpSwitch {
    switch_type: 2,
    inverted: true,
    latch_emulation: false,
    delay_ms: 600,
};

#[test]
fn products_folders_and_links_round_trip_with_stock_root_and_null_versus_empty() {
    let dir = Dir::new();
    let path = dir.database(410, 1);
    let mut db = open(&path);
    let capture = StockProduct {
        id: "a".repeat(64),
        product_type: ProductType::Capture,
        read_only: false,
        raw_data: Some(vec![0, 1, 2]),
        metadata: Some(Vec::new()),
    };
    let folder = db
        .transaction(|tx| {
            tx.put_product(&capture)?;
            let folder = tx.create_folder(&NewFolder {
                product_type: ProductType::Capture,
                folder_type: FolderType::UserRoot,
                name: "My Captures",
                parent: 0,
                read_only: false,
            })?;
            tx.link(folder, &capture.id)?;
            Ok(folder)
        })
        .unwrap();
    drop(db);
    let db = StockDb::open(&path, DbLimits::default(), true).unwrap();
    let view = db.view();
    assert_eq!(view.product(&capture.id).unwrap(), Some(capture.clone()));
    assert_eq!(
        view.root_folder(ProductType::Capture).unwrap(),
        Some(folder)
    );
    assert_eq!(view.folder_products(folder).unwrap(), vec![capture.id]);
    // Stock stores product/folder flags as INTEGER and roots with parent 0.
    let raw = Connection::open(&path).unwrap();
    let types: (String, String, i64) = raw
        .query_row(
            "SELECT typeof(p.readonly), typeof(f.readonly), f.parent FROM product p, folder f",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(types, ("integer".into(), "integer".into(), 0));
}

#[test]
fn failed_transaction_rolls_back_every_write() {
    let dir = Dir::new();
    let mut db = open(&dir.database(410, 1));
    let result = db.transaction(|tx| {
        tx.pin_model("5005")?;
        tx.set_default_params("10012001", &[param(0, "0.5")])?;
        // Unknown product: the whole batch must roll back.
        tx.link(1, "missing")
    });
    assert!(matches!(result, Err(Error::FolderMissing(1))));
    let view = db.view();
    assert!(view.pinned_models().unwrap().is_empty());
    assert!(view.default_param_models().unwrap().is_empty());
}

/// `root_folder` caches its result: each write of the handle, also one rolled back after it
/// read the root inside, must reach the next read.
#[test]
fn root_folder_sees_every_write_of_the_handle() {
    let dir = Dir::new();
    let mut db = open(&dir.database(410, 1));
    let root = |db: &StockDb| db.view().root_folder(ProductType::Ir).unwrap();
    assert_eq!(root(&db), None);
    let created = db.transaction(|tx| tx.boot_maintenance()).unwrap().1;
    assert_eq!(root(&db), Some(created));
    let failed = db.transaction(|tx| {
        tx.delete_folder(created)?;
        assert_eq!(tx.root_folder(ProductType::Ir)?, None);
        Err::<(), _>(Error::Limit)
    });
    assert!(matches!(failed, Err(Error::Limit)));
    assert_eq!(root(&db), Some(created));
    db.transaction(|tx| tx.delete_folder(created)).unwrap();
    assert_eq!(root(&db), None);
}

/// `product_rows` and `product_count` stand in for `product_ids` with `product` of each id:
/// the same rows, and the same error, also for rows that do not read.
#[test]
fn product_rows_and_count_read_as_product_ids_and_product() {
    let dir = Dir::new();
    let path = dir.database(410, 1);
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "INSERT INTO product VALUES ('a', 1, 0, x'01', '{}');
             INSERT INTO product VALUES ('b', 1, 'true', 7, x'02');
             INSERT INTO product VALUES ('c', 1, x'00', NULL, NULL);
             INSERT INTO product VALUES ('d', 1, 1, 'text', 2.5);
             INSERT INTO product VALUES (NULL, 1, 0, NULL, NULL);
             INSERT INTO product VALUES ('e', 2, 0, NULL, NULL);",
        )
        .unwrap();
    let db = open(&path);
    let view = db.view();
    let ids = view.product_ids(ProductType::Ir).unwrap();
    let rows = view.product_rows(ProductType::Ir).unwrap();
    assert_eq!(view.product_count(ProductType::Ir).unwrap(), ids.len());
    assert_eq!(
        rows.iter().map(|(id, _)| id).collect::<Vec<_>>(),
        ids.iter().collect::<Vec<_>>()
    );
    for (id, row) in rows {
        let product = view.product(&id).map(|product| {
            let product = product.unwrap();
            StockProduct {
                raw_data: None,
                ..product
            }
        });
        assert_eq!(format!("{row:?}"), format!("{product:?}"), "{id}");
    }
    Connection::open(&path)
        .unwrap()
        .execute("INSERT INTO product VALUES (x'ff', 1, 0, NULL, NULL)", [])
        .unwrap();
    let ids = view.product_ids(ProductType::Ir).map(|ids| ids.len());
    let count = view.product_count(ProductType::Ir);
    assert_eq!(format!("{count:?}"), format!("{ids:?}"));
}

#[test]
fn model_presets_use_stock_text_booleans_unique_names_and_clean_deletes() {
    let dir = Dir::new();
    let path = dir.database(410, 1);
    let mut db = open(&path);
    let id = db
        .transaction(|tx| {
            let id =
                tx.create_model_preset("Lead", 1173, SWITCH, &[param(0, "1"), param(1, "0.24")])?;
            tx.set_default_model_preset(1173, Some((id, false)))?;
            tx.set_default_exp_switch("10012001", Some(SWITCH))?;
            Ok(id)
        })
        .unwrap();
    let stored: (String, String, String) = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT m.expSwitchInverted, m.expSwitchLatchEmulation, d.isFactory FROM model_presets m, default_model_presets d",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(stored, ("true".into(), "false".into(), "false".into()));
    let view = db.view();
    assert_eq!(view.model_preset(id).unwrap().unwrap().exp_switch, SWITCH);
    assert_eq!(view.default_exp_switch("10012001").unwrap(), Some(SWITCH));
    assert_eq!(view.model_preset_params(id).unwrap()[1].value, "0.24");

    let duplicate = db.transaction(|tx| tx.create_model_preset("Lead", 1173, SWITCH, &[]));
    assert!(matches!(duplicate, Err(Error::DuplicateName(_))));
    db.transaction(|tx| tx.create_model_preset("Lead", 5007, SWITCH, &[]))
        .unwrap();

    db.transaction(|tx| tx.delete_model_preset(id)).unwrap();
    let view = db.view();
    assert!(view.model_preset_params(id).unwrap().is_empty());
    assert!(view.default_model_presets().unwrap().is_empty());
}

#[test]
fn open_checks_schema_and_writes_need_the_coros_4_1_version() {
    let dir = Dir::new();
    let path = dir.database(300, 0);
    assert!(matches!(
        StockDb::open(&path, DbLimits::default(), false),
        Err(Error::StockVersion(SchemaVersion {
            major: 300,
            minor: 0
        }))
    ));
    let mut db = StockDb::open(&path, DbLimits::default(), true).unwrap();
    assert!(matches!(
        db.transaction(|tx| tx.pin_model("1")),
        Err(Error::ReadOnly)
    ));
    drop(db);

    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE pinned_models; CREATE TABLE pinned_models (model TEXT);")
        .unwrap();
    assert!(matches!(
        StockDb::open(&path, DbLimits::default(), true),
        Err(Error::StockSchema(_))
    ));

    let owned = dir.0.join("owned.sqlite");
    drop(Library::create_owned(&owned, Limits::default()).unwrap());
    assert!(matches!(
        StockDb::open(&owned, DbLimits::default(), true),
        Err(Error::StockSchema(_))
    ));
}

/// Factory reset (`0x006a551c` then `0x006a576c`): a new database is at 410.1 and
/// writable; create never replaces a file or ignores a leftover journal.
#[test]
fn create_makes_a_410_1_database_and_never_replaces_one() {
    let dir = Dir::new();
    let path = dir.0.join("zenui.db");
    let mut db = StockDb::create(&path, DbLimits::default()).unwrap();
    assert_eq!(db.version(), COROS_4_1_SCHEMA);
    let (captures, irs) = db.transaction(|tx| tx.boot_maintenance()).unwrap();
    assert_ne!(captures, irs);
    drop(db);
    assert!(matches!(
        StockDb::create(&path, DbLimits::default()),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists
    ));
    fs::remove_file(&path).unwrap();
    fs::write(dir.0.join("zenui.db-journal"), b"").unwrap();
    assert!(matches!(
        StockDb::create(&path, DbLimits::default()),
        Err(Error::UnsafeSidecar(_))
    ));
    assert!(!path.exists());
}

#[test]
fn setlist_files_have_256_slots_with_empty_tokens() {
    let mut slots = vec![None; SETLIST_SLOTS];
    slots[3] = Some("/media/p4/Presets/A/x.pb".to_owned());
    let bytes = encode_setlist(&slots).unwrap();
    assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), SETLIST_SLOTS);
    assert!(bytes.starts_with(b"empty\nempty\nempty\n/media/p4/Presets/A/x.pb\nempty\n"));
    assert_eq!(decode_setlist(&bytes).unwrap(), slots);
    // ZenUI accepts exactly 256 lines; anything else is not a setlist.
    assert!(decode_setlist(b"empty\n").is_err());
    assert!(decode_setlist(&b"empty\n".repeat(SETLIST_SLOTS + 1)).is_err());
    assert!(encode_setlist(&slots[1..]).is_err());
}

#[test]
fn device_paths_and_list_entries_cannot_escape_or_split_lines() {
    let media = MediaRoot::new("/host/copy");
    assert_eq!(
        media.resolve("/media/p4/Presets/My Presets/a.pb").unwrap(),
        Path::new("/host/copy/Presets/My Presets/a.pb")
    );
    for bad in [
        "/media/p4/../etc/passwd",
        "/media/p4/Presets/./a.pb",
        "/media/p40/x",
        "/opt/neuraldsp/x",
        "/media/p4/",
    ] {
        assert!(media.resolve(bad).is_err(), "{bad}");
    }
    assert!(encode_lines(&["a\nb"]).is_err());
    // Stock keeps empty lines (`0x0069a8cc`): pair files must not shift.
    assert_eq!(decode_lines(b"a\n\nb", 10).unwrap(), ["a", "", "b"]);
    assert_eq!(encode_lines(&["a", "", "b"]).unwrap(), b"a\n\nb\n");
    assert!(decode_lines(b"", 10).unwrap().is_empty());
    assert!(matches!(decode_lines(b"a\nb\n", 1), Err(Error::Limit)));

    let dir = Dir::new();
    let file = dir.0.join("favorites");
    write_atomic(&file, b"one\n").unwrap();
    write_atomic(&file, b"two\n").unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"two\n");
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[test]
fn search_index_matches_substrings_case_insensitively_once_per_entry() {
    let entry = |name: &str, path: &str| SearchEntry {
        name: name.to_owned(),
        folder_key: 7,
        path: path.to_owned(),
    };
    let index = SearchIndex::build(vec![
        entry("Brit Plexi", "/media/p4/Presets/A/a.pb"),
        entry("plexi plexi", "/media/p4/Presets/A/b.pb"),
        entry("Clean", "c".repeat(64).as_str()),
        // Stock maps owners by start offset, so `|` in a name is valid.
        entry("Plexi|Lead", "/media/p4/Presets/A/d.pb"),
    ])
    .unwrap();
    let decoded = SearchIndex::decode(&index.encode(), 16).unwrap();
    assert_eq!(decoded, index);
    let names: Vec<_> = decoded
        .search("PLEXI")
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, ["brit plexi", "plexi plexi", "plexi|lead"]);
    assert!(decoded.search("i|p").is_empty());
    assert!(decoded.search("").is_empty());
}

/// Decode checks the stored arrays in place of a second sort: arrays that `build` would not
/// make are refused, also when every position keeps its owner.
#[test]
fn search_index_decode_refuses_arrays_that_build_would_not_make() {
    let entry = SearchEntry {
        name: "abc".into(),
        folder_key: 7,
        path: "/media/p4/Presets/A/a.pb".into(),
    };
    let bytes = SearchIndex::build(vec![entry]).unwrap().encode();
    // Text `abc|` at 2..6, then the suffix array (one byte per position) at 8..12.
    assert_eq!(
        (&bytes[2..6], bytes[6], &bytes[8..12]),
        (&b"abc|"[..], 0x12, &[0, 1, 2, 3][..])
    );
    assert!(SearchIndex::decode(&bytes, 16).is_ok());
    let tampered = |change: fn(&mut Vec<u8>)| {
        let mut bytes = bytes.clone();
        change(&mut bytes);
        SearchIndex::decode(&bytes, 16)
    };
    assert!(tampered(|bytes| bytes.swap(8, 9)).is_err(), "out of order");
    assert!(tampered(|bytes| bytes[9] = 0).is_err(), "a position twice");
    assert!(
        tampered(|bytes| bytes[2] = b'A').is_err(),
        "a name not in lowercase"
    );
}

#[test]
fn ir_product_id_uses_unpadded_lowercase_hex_bytes() {
    let mut md5 = [0xab; 16];
    md5[0] = 0x0f;
    md5[1] = 0x00;
    assert_eq!(ir_product_id(&md5), format!("CIR_f0{}", "ab".repeat(14)));
}

#[test]
fn boot_maintenance_keeps_roots_drops_null_ids_and_is_idempotent() {
    let dir = Dir::new();
    let path = dir.database(410, 1);
    let mut db = open(&path);
    let captures = db
        .transaction(|tx| {
            tx.create_folder(&NewFolder {
                product_type: ProductType::Capture,
                folder_type: FolderType::UserRoot,
                name: "My Captures",
                parent: 0,
                read_only: false,
            })
        })
        .unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "INSERT INTO product VALUES (NULL, 2, 0, NULL, NULL);
             INSERT INTO downloads_product_preset VALUES (NULL, NULL, NULL);
             INSERT INTO downloads_product_nc VALUES (NULL, 'h', NULL, NULL);",
        )
        .unwrap();
    let first = db.transaction(|tx| tx.boot_maintenance()).unwrap();
    assert_eq!(first.0, captures);
    assert_eq!(
        db.view().root_folder(ProductType::Ir).unwrap(),
        Some(first.1)
    );
    let nulls: i64 = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product) + (SELECT count(*) FROM downloads_product_preset)
                  + (SELECT count(*) FROM downloads_product_nc)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(nulls, 0);
    assert_eq!(db.transaction(|tx| tx.boot_maintenance()).unwrap(), first);
    assert_eq!(db.view().folders().unwrap().len(), 2);
}

#[test]
fn legacy_defaults_replace_rows_and_keep_other_params_of_the_model() {
    let dir = Dir::new();
    let mut db = open(&dir.database(410, 1));
    db.transaction(|tx| {
        tx.set_default_params("10012001", &[param(0, "old"), param(1, "kept")])?;
        tx.import_legacy_defaults("10012001", &[param(0, "new"), param(2, "0.5")], SWITCH)
    })
    .unwrap();
    let view = db.view();
    let values: Vec<_> = view
        .default_params("10012001")
        .unwrap()
        .into_iter()
        .map(|p| (p.index, p.value))
        .collect();
    assert_eq!(
        values,
        [(0, "new".into()), (1, "kept".into()), (2, "0.5".into())]
    );
    assert_eq!(view.default_exp_switch("10012001").unwrap(), Some(SWITCH));
}

/// `0x00483b18`: a pre-300 database takes 300.1, 300.2 and 410.1 (downloads
/// renamed, old queues and folders dropped); a 300.2 database takes only 410.1.
#[test]
fn upgrade_applies_the_stock_patch_chain_by_major() {
    let dir = Dir::new();
    let path = dir.0.join("zenui.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE product_preset (id TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY (id));
             CREATE TABLE product_nc (id TEXT, hash TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY (id));
             CREATE TABLE preset_download_queue (id TEXT);
             CREATE TABLE folder (id INTEGER, name TEXT);
             INSERT INTO product_preset VALUES ('cloud-1', x'01', x'02');",
        )
        .unwrap();
    let applied = StockDb::upgrade(&path, DbLimits::default()).unwrap();
    let versions: Vec<_> = applied.iter().map(|v| (v.major, v.minor)).collect();
    assert_eq!(versions, [(300, 1), (300, 2), (410, 1)]);
    let db = StockDb::open(&path, DbLimits::default(), false).unwrap();
    assert_eq!(db.version(), COROS_4_1_SCHEMA);
    let ids = db.view().download_ids(DownloadKind::Preset).unwrap();
    assert_eq!(ids, ["cloud-1"]);
    drop(db);
    assert!(
        StockDb::upgrade(&path, DbLimits::default())
            .unwrap()
            .is_empty()
    );

    let dir = Dir::new();
    let path = dir.database(300, 2);
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DROP TABLE model_preset_parameters; DROP TABLE default_model_presets; DROP TABLE model_presets;",
        )
        .unwrap();
    let applied = StockDb::upgrade(&path, DbLimits::default()).unwrap();
    assert_eq!(applied, [COROS_4_1_SCHEMA]);
    StockDb::open(&path, DbLimits::default(), false).unwrap();
}

/// `0x006a68f4`: only writable captures are rewritten, then the flag is set;
/// a store already in the target is not read again (`0x006a46ec`).
#[test]
fn capture_store_translation_rewrites_writable_captures_then_sets_the_flag() {
    let dir = Dir::new();
    let mut db = StockDb::open(&dir.database(410, 1), DbLimits::default(), false).unwrap();
    let capture = |id: &str, read_only| StockProduct {
        id: id.into(),
        product_type: ProductType::Capture,
        read_only,
        raw_data: Some(vec![1]),
        metadata: Some(vec![2]),
    };
    db.transaction(|write| {
        write.put_product(&capture("user", false))?;
        write.put_product(&capture("factory", true))
    })
    .unwrap();
    let outcome = db
        .translate_capture_store(CaptureStore::User, CaptureEncryption::Local, |row| {
            row.metadata.as_mut().unwrap()[0] = 9;
            Ok(())
        })
        .unwrap();
    assert_eq!(outcome, StoreTranslation::Translated(1));
    let view = db.view();
    assert_eq!(
        view.product("user").unwrap().unwrap().metadata,
        Some(vec![9])
    );
    assert_eq!(
        view.product("factory").unwrap().unwrap().metadata,
        Some(vec![2])
    );
    assert_eq!(
        view.capture_encryption(CaptureStore::User).unwrap(),
        CaptureEncryption::Local
    );
    let again = db
        .translate_capture_store(CaptureStore::User, CaptureEncryption::Local, |_| {
            panic!("an already local store is not read")
        })
        .unwrap();
    assert_eq!(again, StoreTranslation::AlreadyInTarget);
}

#[test]
fn copy_holds_committed_rows_refuses_an_existing_file_and_close_leaves_no_journal() {
    let dir = Dir::new();
    let path = dir.database(410, 1);
    let mut db = StockDb::open(&path, DbLimits::default(), false).unwrap();
    let item = Download {
        id: "cloud".into(),
        hash: Some("hash".into()),
        raw_data: Some(vec![1]),
        metadata: Some(vec![2]),
    };
    db.transaction(|write| write.put_download(DownloadKind::Capture, &item))
        .unwrap();
    let copy = dir.0.join("stage.db");
    db.copy_to(&copy).unwrap();
    assert!(matches!(db.copy_to(&copy), Err(Error::Io(_))));
    db.close().unwrap();
    assert!(!dir.0.join("zenui.db-journal").exists());
    let copied = StockDb::open(&copy, DbLimits::default(), true).unwrap();
    assert_eq!(
        copied
            .view()
            .download(DownloadKind::Capture, "hash")
            .unwrap(),
        Some(item)
    );
}

/// A synthetic database with `megabytes` of 256 KiB capture download blobs (no stock bytes).
fn large_database(dir: &Dir, megabytes: usize) -> PathBuf {
    let path = dir.database(410, 1);
    let mut db = open(&path);
    db.transaction(|write| {
        for index in 0..megabytes * 4 {
            let blob: Vec<u8> = (0..256 * 1024u32)
                .map(|i| (i.wrapping_add(index as u32).wrapping_mul(2_654_435_761) >> 13) as u8)
                .collect();
            let item = Download {
                id: format!("synthetic-{index}"),
                hash: Some(format!("hash-{index}")),
                raw_data: Some(blob),
                metadata: None,
            };
            write.put_download(DownloadKind::Capture, &item)?;
        }
        Ok(())
    })
    .unwrap();
    db.close().unwrap();
    path
}

/// One small device commit (the stock 500 ms busy timeout of [`StockDb`]).
fn device_commit(db: &mut StockDb, index: usize) -> Result<()> {
    let item = Download {
        id: format!("device-{index}"),
        hash: Some(format!("device-hash-{index}")),
        raw_data: Some(vec![1; 64]),
        metadata: None,
    };
    db.transaction(|write| write.put_download(DownloadKind::Capture, &item))
}

/// Review r19 F4: a device commit lands while a backup stage copy runs, and the copy then
/// holds it (the copy restarts on a change from another connection). `VACUUM INTO` held the
/// read lock for the whole copy, so the commit waited for the copy's end.
#[test]
fn a_device_commit_lands_during_a_stage_copy_and_the_copy_holds_it() {
    let dir = Dir::new();
    let path = large_database(&dir, 48);
    let copy = dir.0.join("stage.db");
    let mut device = open(&path);
    let stage = StockDb::open(&path, DbLimits::default(), true).unwrap();
    let copying = {
        let copy = copy.clone();
        std::thread::spawn(move || stage.copy_to(&copy))
    };
    while !copy.exists() {
        std::thread::yield_now();
    }
    std::thread::sleep(Duration::from_millis(5));
    device_commit(&mut device, 0).unwrap();
    assert!(
        !copying.is_finished(),
        "the commit landed before the copy ended"
    );
    copying.join().unwrap().unwrap();
    let copied = StockDb::open(&copy, DbLimits::default(), true).unwrap();
    let held = copied
        .view()
        .download(DownloadKind::Capture, "device-hash-0");
    assert!(held.unwrap().is_some());
}

/// Review r20 G7: commits from another connection faster than one copy step restart the copy
/// right after its first step every time, so the copied count stays equal. Each of those
/// restarts counts, and the copy gives up instead of looping until the writes stop.
#[test]
fn a_copy_restart_right_after_the_first_step_counts() {
    use rusqlite::backup::Progress;
    let at = |copied| Progress {
        remaining: 1_000 - copied,
        pagecount: 1_000,
    };
    let mut restarts = CopyRestarts::default();
    restarts.step(at(256)).unwrap();
    restarts.step(at(512)).unwrap();
    for _ in 0..COPY_RESTARTS {
        restarts.step(at(256)).unwrap();
    }
    assert!(matches!(restarts.step(at(256)), Err(Error::CopyContended)));
}

/// Review r19 F4 measurement (slow; run with `--ignored --nocapture`; `TETRAD_COPY_MB` sets the
/// size, `TETRAD_COMMIT_MS` the writer period): copy time and result, and the longest commit
/// and failed commits of a device writer during `VACUUM INTO` and during
/// [`StockDb::copy_to`].
#[test]
#[ignore = "measurement"]
fn measure_stage_copy_against_device_commits() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;
    let megabytes = std::env::var("TETRAD_COPY_MB").map_or(256, |mb| mb.parse().unwrap());
    let period = std::env::var("TETRAD_COMMIT_MS").map_or(10, |ms| ms.parse().unwrap());
    let dir = Dir::new();
    let path = large_database(&dir, megabytes);
    for (method, stepped) in [("VACUUM INTO", false), ("copy_to", true)] {
        let copy = dir.0.join(format!("stage-{stepped}.db"));
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let (done, path) = (done.clone(), path.clone());
            std::thread::spawn(move || {
                let mut db = open(&path);
                let (mut longest, mut failed, mut index) = (Duration::ZERO, 0, 0);
                while !done.load(Ordering::Relaxed) {
                    let start = Instant::now();
                    failed += usize::from(device_commit(&mut db, index).is_err());
                    longest = longest.max(start.elapsed());
                    index += 1;
                    std::thread::sleep(Duration::from_millis(period));
                }
                (longest, failed, index)
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        let start = Instant::now();
        let copied = if stepped {
            let stage = StockDb::open(&path, DbLimits::default(), true).unwrap();
            stage.copy_to(&copy).map_err(|error| error.to_string())
        } else {
            let source = Connection::open(&path).unwrap();
            let copied = source.execute("VACUUM INTO ?1", [copy.to_str().unwrap()]);
            copied.map(drop).map_err(|error| error.to_string())
        };
        let took = start.elapsed();
        done.store(true, Ordering::Relaxed);
        let (longest, failed, commits) = writer.join().unwrap();
        println!(
            "{megabytes} MB, commit every {period} ms, {method}: copy {took:?} {copied:?}, \
             {commits} commits, longest {longest:?}, failed {failed}"
        );
    }
}

/// r25-ui MR 3: the quarantine keeps the dialog name. A table written before the `name`
/// column reads its id as name, and the next quarantine adds the column and names the old
/// rows by their id. Such a table has the zen-rs name; the open renames it first.
#[test]
fn the_capture_quarantine_keeps_names_and_migrates_an_older_table() {
    let dir = Dir::new();
    let path = dir.0.join("zenui.db");
    let mut db = StockDb::create(&path, DbLimits::default()).unwrap();
    db.transaction(|write| {
        write.boot_maintenance()?;
        write.put_product(&StockProduct {
            id: "bad".into(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(b"raw".to_vec()),
            metadata: Some(b"meta".to_vec()),
        })
    })
    .unwrap();
    drop(db);
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE zen_capture_quarantine (store TEXT NOT NULL, id TEXT NOT NULL, hash \
             TEXT, rawdata BLOB, metadata BLOB, reason TEXT NOT NULL); INSERT INTO \
             zen_capture_quarantine VALUES ('user', 'old', NULL, NULL, NULL, 'cut write')",
        )
        .unwrap();
    let mut db = open(&path);
    let names = |db: &StockDb| -> Vec<(String, String)> {
        (db.view().quarantined_captures().unwrap().into_iter())
            .map(|row| (row.id, row.name))
            .collect()
    };
    assert_eq!(names(&db), [("old".into(), "old".into())]);
    db.transaction(|write| write.quarantine_capture(CaptureStore::User, "bad", "Crunch", "V1"))
        .unwrap();
    assert_eq!(
        names(&db),
        [
            ("old".into(), "old".into()),
            ("bad".into(), "Crunch".into())
        ]
    );
}

/// The zen-rs to Tetrad rename: a writable open moves `zen_capture_quarantine` to
/// `tetrad_capture_quarantine`. Old only: renamed. Both: the old rows the new table does not
/// hold are copied, then the old table is dropped. New only and neither: no change. A
/// read-only open changes nothing, and a second writable open finds nothing to do.
#[test]
fn a_writable_open_migrates_the_zen_rs_quarantine_table() {
    const OLD: &str = "CREATE TABLE zen_capture_quarantine (store TEXT NOT NULL, id TEXT NOT \
                       NULL, hash TEXT, rawdata BLOB, metadata BLOB, reason TEXT NOT NULL); \
                       INSERT INTO zen_capture_quarantine VALUES ('user', 'dup', NULL, x'01', \
                       NULL, 'cut write'), ('user', 'old', NULL, x'02', NULL, 'cut write');";
    const NEW: &str = "CREATE TABLE tetrad_capture_quarantine (store TEXT NOT NULL, id TEXT NOT \
                       NULL, hash TEXT, rawdata BLOB, metadata BLOB, reason TEXT NOT NULL, name \
                       TEXT); INSERT INTO tetrad_capture_quarantine VALUES ('user', 'dup', NULL, \
                       x'01', NULL, 'cut write', 'Dup');";
    let tables = |path: &Path| -> Vec<String> {
        let connection = Connection::open(path).unwrap();
        let mut statement = connection
            .prepare(
                "SELECT name FROM sqlite_master WHERE name LIKE '%capture_quarantine' ORDER BY \
                 name",
            )
            .unwrap();
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap();
        names.map(|name| name.unwrap()).collect()
    };
    let rows = |db: &StockDb| -> Vec<(String, String)> {
        (db.view().quarantined_captures().unwrap().into_iter())
            .map(|row| (row.id, row.name))
            .collect()
    };
    let pair = |id: &str, name: &str| (id.to_owned(), name.to_owned());
    let both = format!("{OLD}{NEW}");
    let cases = [
        ("", vec![], vec![]),
        (
            OLD,
            vec!["tetrad_capture_quarantine"],
            vec![pair("dup", "dup"), pair("old", "old")],
        ),
        (
            both.as_str(),
            vec!["tetrad_capture_quarantine"],
            vec![pair("dup", "Dup"), pair("old", "old")],
        ),
        (
            NEW,
            vec!["tetrad_capture_quarantine"],
            vec![pair("dup", "Dup")],
        ),
    ];
    for (setup, want_tables, want_rows) in cases {
        let dir = Dir::new();
        let path = dir.0.join("zenui.db");
        drop(StockDb::create(&path, DbLimits::default()).unwrap());
        Connection::open(&path)
            .unwrap()
            .execute_batch(setup)
            .unwrap();
        let before = tables(&path);
        drop(StockDb::open(&path, DbLimits::default(), true).unwrap());
        assert_eq!(tables(&path), before, "{setup}");
        for _ in 0..2 {
            let db = open(&path);
            assert_eq!(tables(&path), want_tables, "{setup}");
            assert_eq!(rows(&db), want_rows, "{setup}");
        }
    }
}

/// A3: a Downloads listing reads the keys (the columns before `rawdata`, no overflow walk) and
/// then the metadata of a rowid range. Both skip nothing the full row read gives, and a NULL id
/// row is not listed.
#[test]
fn download_keys_and_metadata_ranges_match_the_full_rows() {
    let dir = Dir::new();
    let path = dir.database(410, 1);
    let mut db = open(&path);
    let row = |index: u8| Download {
        id: format!("cloud-{index}"),
        hash: Some(format!("hash-{index}")),
        raw_data: Some(vec![index; 10_000]),
        metadata: (index != 2).then(|| vec![index; 600]),
    };
    db.transaction(|write| {
        (1..=4).try_for_each(|index| write.put_download(DownloadKind::Capture, &row(index)))
    })
    .unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch("INSERT INTO downloads_product_nc VALUES (NULL, 'h', x'00', x'01');")
        .unwrap();
    let view = db.view();
    let keys = view.download_keys(DownloadKind::Capture).unwrap();
    let ids: Vec<_> = keys
        .iter()
        .map(|key| (key.id.as_str(), key.hash.as_deref()))
        .collect();
    assert_eq!(
        ids,
        [
            ("cloud-1", Some("hash-1")),
            ("cloud-2", Some("hash-2")),
            ("cloud-3", Some("hash-3")),
            ("cloud-4", Some("hash-4")),
        ]
    );
    let (second, third) = (keys[1].rowid, keys[2].rowid);
    let metadata = view
        .download_metadata(DownloadKind::Capture, second..=third)
        .unwrap();
    assert_eq!(metadata, [(second, None), (third, row(3).metadata)]);
    let listing = view.download_listing(DownloadKind::Capture).unwrap();
    assert!(
        (listing.iter().zip(&keys)).all(|(full, key)| full.id == key.id && full.hash == key.hash)
    );
}
