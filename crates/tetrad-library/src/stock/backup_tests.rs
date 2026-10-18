//! Backup re-key, restore reopen and Downloads store, on synthetic databases and ciphers.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tetrad_media::factory::DeviceCipher;
use tetrad_storage::stock::{
    CaptureEncryption, CaptureStore, DbLimits, Download, DownloadKind, ProductType, StockDb,
    StockProduct, StoreTranslation,
};

use super::*;
use crate::Error;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "tetrad-library-backup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

const RAW: &[u8] = &[1, 2, 3, 4];
/// Version "1": the rawdata is re-keyed too (`0x006a60e8`).
const METADATA: &[u8] = br#"{"version":"1"}"#;
const USER_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn global() -> DeviceCipher {
    DeviceCipher::caller_secret(b"synthetic seed-only cipher")
}

fn local() -> DeviceCipher {
    DeviceCipher::caller_secret(b"synthetic seed-and-serial cipher")
}

fn sealed(cipher: &DeviceCipher, plain: &[u8]) -> Vec<u8> {
    let mut bytes = plain.to_vec();
    cipher.apply(&mut bytes);
    bytes
}

/// A new database with one user capture and one downloaded capture in `cipher`, both
/// stores flagged `flag`.
fn database(path: &Path, cipher: &DeviceCipher, flag: CaptureEncryption) -> StockDb {
    let mut db = StockDb::create(path, DbLimits::default()).unwrap();
    db.transaction(|write| {
        write.put_product(&StockProduct {
            id: USER_ID.into(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(sealed(cipher, RAW)),
            metadata: Some(sealed(cipher, METADATA)),
        })?;
        write.put_download(
            DownloadKind::Capture,
            &Download {
                id: "cloud".into(),
                hash: Some("hash".into()),
                raw_data: Some(sealed(cipher, RAW)),
                metadata: Some(sealed(cipher, METADATA)),
            },
        )?;
        write.set_capture_encryption(CaptureStore::User, flag)?;
        write.set_capture_encryption(CaptureStore::Downloads, flag)
    })
    .unwrap();
    db
}

/// True when both capture rows are in `cipher` and both stores are flagged `flag`.
fn keyed(db: &StockDb, cipher: &DeviceCipher, flag: CaptureEncryption) -> bool {
    let view = db.view();
    let user = view.product(USER_ID).unwrap().unwrap();
    let download = view
        .download(DownloadKind::Capture, "cloud")
        .unwrap()
        .unwrap();
    let expected = (Some(sealed(cipher, RAW)), Some(sealed(cipher, METADATA)));
    (user.raw_data, user.metadata) == expected
        && (download.raw_data, download.metadata) == expected
        && [CaptureStore::User, CaptureStore::Downloads]
            .iter()
            .all(|store| view.capture_encryption(*store).unwrap() == flag)
}

#[test]
fn backup_rekey_moves_a_staged_copy_to_global_and_back() {
    let dir = Dir::new();
    let (global, local) = (global(), local());
    let ciphers = CaptureCiphers {
        global: &global,
        local: &local,
    };
    let live = database(&dir.0.join("zenui.db"), &local, CaptureEncryption::Local);
    let stage = dir.0.join("stage.db");
    live.copy_to(&stage).unwrap();
    let both = [StoreTranslation::Translated(1); 2];
    assert_eq!(
        rekey_for_backup(&stage, Rekey::ForBackup, ciphers).unwrap(),
        both
    );
    let open = |path: &Path| StockDb::open(path, DbLimits::default(), true).unwrap();
    assert!(keyed(&open(&stage), &global, CaptureEncryption::Global));
    assert!(keyed(&live, &local, CaptureEncryption::Local));
    assert_eq!(
        rekey_for_backup(&stage, Rekey::AfterRestore, ciphers).unwrap(),
        both
    );
    assert!(keyed(&open(&stage), &local, CaptureEncryption::Local));
}

/// Review r25 Q6: a quarantined capture follows its store through the backup re-key, so after a
/// restore on another unit (another local cipher) it comes back in that unit's cipher.
#[test]
fn a_quarantined_capture_follows_its_store_through_a_backup() {
    let dir = Dir::new();
    let (global, local) = (global(), local());
    let other = DeviceCipher::caller_secret(b"synthetic cipher of another unit");
    let mut db = database(&dir.0.join("zenui.db"), &local, CaptureEncryption::Local);
    (db.transaction(|write| write.quarantine_capture(CaptureStore::User, USER_ID, "Amp", "V1")))
        .unwrap();
    let ciphers = |local| CaptureCiphers {
        global: &global,
        local,
    };
    rekey(&mut db, Rekey::ForBackup, ciphers(&local)).unwrap();
    rekey(&mut db, Rekey::AfterRestore, ciphers(&other)).unwrap();
    (db.transaction(|write| write.restore_quarantined_capture(CaptureStore::User, USER_ID)))
        .unwrap();
    let back = db.view().product(USER_ID).unwrap().unwrap();
    assert_eq!(
        (back.raw_data, back.metadata),
        (Some(sealed(&other, RAW)), Some(sealed(&other, METADATA)))
    );
}

/// Review r18 F8: the backup stage comes from the file through its own connection while the
/// live handle stays open (the worker cannot borrow the device's handle). Only the copy is
/// re-keyed, and the next stage replaces the old one.
#[test]
fn a_backup_stage_is_copied_and_rekeyed_beside_the_open_live_database() {
    let dir = Dir::new();
    let (global, local) = (global(), local());
    let ciphers = CaptureCiphers {
        global: &global,
        local: &local,
    };
    let path = dir.0.join("zenui.db");
    let live = database(&path, &local, CaptureEncryption::Local);
    let stage = dir.0.join("BackupTmpDir");
    for _ in 0..2 {
        let (staged, rekeyed) = stage_for_backup(&path, &stage, Some(ciphers)).unwrap();
        assert_eq!(staged, stage.join("zenui.db"));
        assert_eq!(
            rekeyed.unwrap().unwrap(),
            [StoreTranslation::Translated(1); 2]
        );
        let copy = StockDb::open(&staged, DbLimits::default(), true).unwrap();
        assert!(keyed(&copy, &global, CaptureEncryption::Global));
        assert!(keyed(&live, &local, CaptureEncryption::Local));
    }
}

#[test]
fn a_restore_closes_then_reopens_the_database_with_local_captures() {
    let dir = Dir::new();
    let (global, local) = (global(), local());
    let path = dir.0.join("zenui.db");
    let backup = dir.0.join("backup.db");
    database(&backup, &global, CaptureEncryption::Global)
        .close()
        .unwrap();
    database(&path, &local, CaptureEncryption::Local)
        .close()
        .unwrap();
    // The extraction replaces the closed file.
    fs::rename(&backup, &path).unwrap();
    let ciphers = CaptureCiphers {
        global: &global,
        local: &local,
    };
    let reopened = reopen_after_restore(&path, DbLimits::default(), Some(ciphers)).unwrap();
    assert!(!reopened.created && reopened.upgraded.is_empty());
    assert_eq!(
        reopened.rekeyed.unwrap().unwrap(),
        [StoreTranslation::Translated(1); 2]
    );
    assert!(keyed(&reopened.db, &local, CaptureEncryption::Local));
    assert!(
        reopened
            .db
            .view()
            .root_folder(ProductType::Capture)
            .unwrap()
            .is_some()
    );
    reopened.db.close().unwrap();

    // A backup without `zenui.db` leaves none: a new one is made.
    fs::remove_file(&path).unwrap();
    let reopened = reopen_after_restore(&path, DbLimits::default(), None).unwrap();
    assert!(reopened.created && reopened.rekeyed.is_none());
}

/// Review r18 F7: a failed boot maintenance (here a write lock held by another connection)
/// keeps the reopened database, so captures and IRs still play.
#[test]
fn a_failed_boot_maintenance_keeps_the_reopened_database() {
    let dir = Dir::new();
    let path = dir.0.join("zenui.db");
    database(&path, &local(), CaptureEncryption::Local)
        .close()
        .unwrap();
    let mut holder = StockDb::open(&path, DbLimits::default(), false).unwrap();
    holder
        .transaction(|_| {
            let reopened = reopen_after_restore(&path, DbLimits::default(), None).unwrap();
            assert!(reopened.maintenance.is_err());
            assert_eq!(
                reopened.db.view().product_ids(ProductType::Capture)?.len(),
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn downloads_folders_have_the_stock_keys() {
    let keys = DOWNLOADS_FOLDERS.map(|folder| (folder.key, folder.file_type));
    assert_eq!(keys, [("cloud-0-1", 0), ("cloud-2-1", 2)]);
    assert_eq!(
        downloads_folder("cloud-2-1").map(|folder| folder.kind),
        Some(DownloadKind::Capture)
    );
    // `cloud-<type>-0` is the cloud browse folder, not Downloads.
    assert_eq!(downloads_folder("cloud-2-0"), None);
}

/// `0x22cab0`: a Downloads folder above 512 items shows the dialog, captures before presets,
/// unless its "Don't show again" flag is set.
#[test]
fn excessive_downloads_pick_captures_first_unless_suppressed() {
    let dir = Dir::new();
    let mut db = StockDb::create(&dir.0.join("zenui.db"), DbLimits::default()).unwrap();
    let fill = |db: &mut StockDb, kind, count| {
        db.transaction(|write| {
            for index in 0..count {
                let item = Download {
                    id: format!("{kind:?}-{index}"),
                    hash: (kind == DownloadKind::Capture).then(|| format!("hash-{index}")),
                    raw_data: Some(vec![1]),
                    metadata: None,
                };
                write.put_download(kind, &item)?;
            }
            Ok(())
        })
        .unwrap();
    };
    let none = |_| false;
    fill(&mut db, DownloadKind::Preset, EXCESSIVE_DOWNLOADS);
    assert_eq!(
        excessive_downloads(&db, none).unwrap(),
        None,
        "512 is not excessive"
    );
    fill(&mut db, DownloadKind::Preset, EXCESSIVE_DOWNLOADS + 1);
    fill(&mut db, DownloadKind::Capture, EXCESSIVE_DOWNLOADS + 1);
    assert_eq!(
        excessive_downloads(&db, none).unwrap(),
        Some(DownloadKind::Capture)
    );
    let captures_hidden = |kind| kind == DownloadKind::Capture;
    assert_eq!(
        excessive_downloads(&db, captures_hidden).unwrap(),
        Some(DownloadKind::Preset)
    );
    assert_eq!(excessive_downloads(&db, |_| true).unwrap(), None);
}

#[test]
fn a_cloud_capture_is_stored_only_into_a_local_downloads_store() {
    let dir = Dir::new();
    let mut db = StockDb::create(&dir.0.join("zenui.db"), DbLimits::default()).unwrap();
    let capture = Download {
        id: "cloud".into(),
        hash: Some("hash".into()),
        raw_data: Some(RAW.to_vec()),
        metadata: Some(METADATA.to_vec()),
    };
    assert_eq!(
        store_download(&mut db, DownloadKind::Capture, &capture).unwrap(),
        StoredDownload::Added
    );
    assert_eq!(
        store_download(&mut db, DownloadKind::Capture, &capture).unwrap(),
        StoredDownload::Replaced
    );
    assert_eq!(
        db.view().download_ids(DownloadKind::Capture).unwrap(),
        ["cloud"]
    );

    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::Downloads, CaptureEncryption::Global)
    })
    .unwrap();
    let other = Download {
        id: "other".into(),
        ..capture
    };
    assert!(matches!(
        store_download(&mut db, DownloadKind::Capture, &other),
        Err(Error::Unsupported(_))
    ));
    // Presets have no store key and no hash.
    let preset = Download {
        hash: None,
        ..other
    };
    assert_eq!(
        store_download(&mut db, DownloadKind::Preset, &preset).unwrap(),
        StoredDownload::Added
    );
}
