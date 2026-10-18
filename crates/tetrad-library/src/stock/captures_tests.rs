//! Capture manager steps (`0x703900`, `0x702204`, `0x702f70`) on synthetic files, rows and ciphers.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tetrad_media::factory::DeviceCipher;
use tetrad_storage::media::{MediaRoot, SETLIST_SLOTS, encode_setlist};
use tetrad_storage::stock::{
    CaptureStore, DbLimits, Download, DownloadKind, ProductType, QuarantinedCapture, StockDb,
    StockProduct,
};

use super::*;
use tetrad_services::cloud::products::{CloudProduct, ProductPayload};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A synthetic `/media/p4` with "My Presets" and `Captures/My Captures`.
struct Media(PathBuf);

impl Media {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-library-captures-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("Presets/My Presets")).unwrap();
        fs::create_dir_all(root.join("Captures/My Captures")).unwrap();
        fs::write(
            root.join("Presets/My Presets/setlist"),
            encode_setlist(&vec![None; SETLIST_SLOTS]).unwrap(),
        )
        .unwrap();
        Self(root)
    }

    fn legacy(&self, file: &str) -> PathBuf {
        self.0.join("Captures/My Captures").join(file)
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn sealed(cipher: &DeviceCipher, plain: &[u8]) -> Vec<u8> {
    let mut bytes = plain.to_vec();
    cipher.apply(&mut bytes);
    bytes
}

/// `0x703900` re-keys both files of a capture from the seed-only cipher to the device
/// cipher; the conversion step names the captures without a sidecar and changes no file.
/// Review r23 K4: a run cut before its journal line stages again, and a journaled run cut
/// before its commit commits again; neither re-keys a file twice.
#[test]
fn legacy_capture_files_move_to_the_device_cipher_and_unconverted_ones_stay() {
    let media = Media::new();
    let global = DeviceCipher::caller_secret(b"synthetic seed-only cipher");
    let local = DeviceCipher::caller_secret(b"synthetic seed-and-serial cipher");
    fs::write(media.legacy("a.cns"), sealed(&global, b"model")).unwrap();
    fs::write(media.legacy("a.json"), sealed(&global, b"{}")).unwrap();
    fs::write(media.legacy("old.cns"), b"format 0 file").unwrap();
    let library = StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default()).unwrap();

    assert_eq!(library.legacy_capture_names().unwrap(), ["a", "old"]);
    for _ in 0..2 {
        library.stage_legacy_capture("a", &global, &local).unwrap();
    }
    assert_eq!(
        fs::read(media.legacy("a.cns")).unwrap(),
        sealed(&global, b"model"),
        "staging leaves the file"
    );
    for _ in 0..2 {
        library.commit_legacy_capture("a").unwrap();
    }
    assert!(!media.legacy(&format!("a.cns{REKEYED_SUFFIX}")).exists());
    assert_eq!(
        fs::read(media.legacy("a.cns")).unwrap(),
        sealed(&local, b"model")
    );
    assert_eq!(
        fs::read(media.legacy("a.json")).unwrap(),
        sealed(&local, b"{}")
    );

    assert_eq!(library.unconverted_legacy_captures().unwrap(), ["old.cns"]);
    assert_eq!(fs::read(media.legacy("old.cns")).unwrap(), b"format 0 file");
}

/// A synthetic 0x2864-byte V1 parameter block (1→2→2→1) with a `-0.0` post gain.
fn params_block() -> Vec<u8> {
    let mut out = Vec::new();
    let mut words = |words: &[u32]| out.extend(words.iter().flat_map(|word| word.to_le_bytes()));
    words(&[48_000f32.to_bits(), 512, 2048]);
    words(
        &(0..2560)
            .map(|i| (i as f32 / 4096.0).to_bits())
            .collect::<Vec<_>>(),
    );
    words(&[0.75f32.to_bits(), (-0.0f32).to_bits(), 3, 1, 2, 2, 1]);
    words(
        &(0..13)
            .map(|i| (i as f32 - 6.5).to_bits())
            .collect::<Vec<_>>(),
    );
    words(&[20f32.to_bits(), 0.7f32.to_bits()]);
    assert_eq!(out.len(), 0x2864);
    out
}

/// A format-0 file (`0x6fa77c`): magic, format word 0, header word, name, author, tags,
/// then the block size word at `h + 7` and the block.
fn format_zero(params: &[u8]) -> Vec<u8> {
    let mut out = b"CNS\0\0\0\0".to_vec();
    out.extend(0u32.to_be_bytes());
    out.extend(0u32.to_be_bytes());
    let text = |out: &mut Vec<u8>, text: &str| {
        out.extend((text.len() as u32).to_be_bytes());
        out.extend(text.as_bytes());
    };
    text(&mut out, "Old Amp");
    text(&mut out, "Someone|user-1");
    out.extend(2u32.to_be_bytes());
    text(&mut out, "crunch");
    text(&mut out, "lead");
    let header = out.len() - 7;
    out[0xb..0xf].copy_from_slice(&(header as u32).to_be_bytes());
    out.extend(0x2864u32.to_be_bytes());
    out.extend(params);
    out
}

/// `0x702204` → `0x6ff72c` (O: stage, verify, commit, verify, remove): a format-0 capture
/// becomes `<hash>.cns` and `<hash>.json` in the device cipher that the V1 loader reads back
/// as the old block (`-0.0` scalars become `0.0`, as protobuf 3.18 writes them). The old file
/// stays through staging, staging again and a cut before the commit; a second commit is a
/// no-op. A file in another format is kept as it is.
#[test]
fn a_format_zero_capture_converts_once_and_its_file_goes_last() {
    let media = Media::new();
    let local = DeviceCipher::caller_secret(b"synthetic seed-and-serial cipher");
    let params = params_block();
    fs::write(
        media.legacy("Old.cns"),
        sealed(&local, &format_zero(&params)),
    )
    .unwrap();
    fs::write(media.legacy("v1.cns"), sealed(&local, b"not format 0")).unwrap();
    let library = StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default()).unwrap();
    assert_eq!(
        library.unconverted_legacy_captures().unwrap(),
        ["Old.cns", "v1.cns"]
    );

    let mut staged = Vec::new();
    for _ in 0..2 {
        staged.push(library.stage_legacy_conversion("Old.cns", &local).unwrap());
    }
    assert_eq!(staged[0], staged[1]);
    let LegacyConversion::Staged { hash } = staged.remove(0) else {
        panic!("format 0 converts: {staged:?}");
    };
    assert!(
        media.legacy("Old.cns").exists(),
        "staging leaves the old file"
    );
    assert!(!media.legacy(&format!("{hash}.cns")).exists());
    for _ in 0..2 {
        assert_eq!(
            library
                .commit_legacy_conversion("Old.cns", &hash, &local)
                .unwrap(),
            LegacyCommit::Committed
        );
    }
    assert!(!media.legacy("Old.cns").exists());
    assert!(
        !media
            .legacy(&format!("{hash}.cns{CONVERTED_SUFFIX}"))
            .exists()
    );

    let mut record = fs::read(media.legacy(&format!("{hash}.cns"))).unwrap();
    local.apply(&mut record);
    let mut expected = params.clone();
    expected[0x2810..0x2814].fill(0);
    assert_eq!(
        tetrad_capture::stored::v1_runtime_data(&record).unwrap(),
        expected
    );
    let mut json = fs::read(media.legacy(&format!("{hash}.json"))).unwrap();
    local.apply(&mut json);
    let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
    for (key, value) in [
        ("version", "1"),
        ("name", "Old"),
        ("hash", hash.as_str()),
        ("authorName", "Someone"),
        ("authorId", "user-1"),
        ("encryption", "Local"),
        ("parametersBlockLength", "10340"),
        ("deviceType", "default"),
    ] {
        assert_eq!(json[key], value, "{key}");
    }
    assert_eq!(json["tags"], serde_json::json!(["crunch", "lead"]));

    assert!(matches!(
        library.stage_legacy_conversion("v1.cns", &local).unwrap(),
        LegacyConversion::Kept(_)
    ));
    assert_eq!(
        fs::read(media.legacy("v1.cns")).unwrap(),
        sealed(&local, b"not format 0")
    );
    assert_eq!(library.unconverted_legacy_captures().unwrap(), ["v1.cns"]);
}

/// Review r25 Q1: a staged record damaged after its journal line is checked before any
/// rename. The commit keeps the old file, leaves no `<hash>.*` and no staged copy, and says
/// `Kept`, so the boot step names it and goes on (it was an error that stopped every boot).
#[test]
fn a_staged_capture_that_does_not_read_back_is_kept_before_any_rename() {
    let media = Media::new();
    let local = DeviceCipher::caller_secret(b"synthetic seed-and-serial cipher");
    fs::write(
        media.legacy("Old.cns"),
        sealed(&local, &format_zero(&params_block())),
    )
    .unwrap();
    let library = StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default()).unwrap();
    let LegacyConversion::Staged { hash } =
        library.stage_legacy_conversion("Old.cns", &local).unwrap()
    else {
        panic!("format 0 converts");
    };
    let staged = media.legacy(&format!("{hash}.cns{CONVERTED_SUFFIX}"));
    let mut record = fs::read(&staged).unwrap();
    let last = record.len() - 1;
    record[last] ^= 0x40;
    fs::write(&staged, record).unwrap();

    let commit = library.commit_legacy_conversion("Old.cns", &hash, &local);
    assert!(matches!(commit, Ok(LegacyCommit::Kept(_))), "{commit:?}");
    assert!(media.legacy("Old.cns").exists());
    for file in ["cns", "json"].map(|ext| format!("{hash}.{ext}")) {
        assert!(!media.legacy(&file).exists(), "{file}");
        assert!(!media.legacy(&format!("{file}{CONVERTED_SUFFIX}")).exists());
    }
    assert_eq!(
        library
            .commit_legacy_conversion("Old.cns", &hash, &local)
            .unwrap(),
        LegacyCommit::NotStaged,
        "a later boot stages again"
    );
}

/// Review r25 Q9: a NaN sample rate fails the 48 kHz check (`(48_000 - NaN).abs() > tol` is
/// false, so the old test let it through).
#[test]
fn a_nan_sample_rate_does_not_convert() {
    let mut params = params_block();
    params[..4].copy_from_slice(&f32::NAN.to_le_bytes());
    let reason = conversion::parse_format_zero(&format_zero(&params)).err();
    assert!(reason.is_some_and(|reason| reason.starts_with("samplerate")));
}

/// Review r25 Q8: when `<hash>.*` already holds the same block (converted or imported
/// before), the commit keeps that pair and its sidecar; only the legacy file goes.
#[test]
fn a_conversion_keeps_an_existing_pair_of_the_same_block() {
    let media = Media::new();
    let local = DeviceCipher::caller_secret(b"synthetic seed-and-serial cipher");
    fs::write(
        media.legacy("Old.cns"),
        sealed(&local, &format_zero(&params_block())),
    )
    .unwrap();
    let library = StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default()).unwrap();
    let LegacyConversion::Staged { hash } =
        library.stage_legacy_conversion("Old.cns", &local).unwrap()
    else {
        panic!("format 0 converts");
    };
    let target = |ext: &str| media.legacy(&format!("{hash}.{ext}"));
    let staged = |ext: &str| media.legacy(&format!("{hash}.{ext}{CONVERTED_SUFFIX}"));
    fs::copy(staged("cns"), target("cns")).unwrap();
    let mut json = fs::read(staged("json")).unwrap();
    local.apply(&mut json);
    let mut sidecar: serde_json::Value = serde_json::from_slice(&json).unwrap();
    sidecar["name"] = "Renamed".into();
    let mut json = serde_json::to_vec(&sidecar).unwrap();
    local.apply(&mut json);
    fs::write(target("json"), &json).unwrap();

    assert_eq!(
        library
            .commit_legacy_conversion("Old.cns", &hash, &local)
            .unwrap(),
        LegacyCommit::Committed
    );
    assert_eq!(
        fs::read(target("json")).unwrap(),
        json,
        "the existing sidecar stays"
    );
    assert!(!media.legacy("Old.cns").exists());
    assert!(!staged("cns").exists() && !staged("json").exists());
}

/// `0x702f70`: Downloads before user rows; a bad metadata row is listed by id, a bad rawdata
/// row by name; both leave their stores (with their folder link) for the quarantine table
/// with their bytes. An unchecked row stays.
#[test]
fn corrupt_captures_leave_the_library_for_the_quarantine() {
    let media = Media::new();
    let mut db = StockDb::create(&media.0.join("zenui.db"), DbLimits::default()).unwrap();
    let user = |id: &str, raw: &[u8]| StockProduct {
        id: id.into(),
        product_type: ProductType::Capture,
        read_only: false,
        raw_data: Some(raw.to_vec()),
        metadata: Some(b"meta".to_vec()),
    };
    db.transaction(|write| {
        let (root, _) = write.boot_maintenance()?;
        let good = (0..6).map(|n| user(&format!("good{n}"), b"ok"));
        let rows = [user("bad-raw", b"bad")].into_iter().chain(good);
        for row in rows.chain([user("nc2", b"nc2")]) {
            write.put_product(&row)?;
            write.link(root, &row.id)?;
        }
        write.put_download(
            DownloadKind::Capture,
            &Download {
                id: "cloud".into(),
                hash: Some("key".into()),
                raw_data: Some(b"ok".to_vec()),
                metadata: None,
            },
        )
    })
    .unwrap();

    let check = quarantine_corrupt_captures(&mut db, |raw, metadata| match (raw, metadata) {
        (_, b"") => CaptureCheck::BadMetadata("not a JSON object".into()),
        (b"bad", _) => CaptureCheck::BadRawdata {
            name: "Crunch".into(),
            reason: "V1 runtime data".into(),
        },
        (b"nc2", _) => CaptureCheck::Unchecked("no NC2 key".into()),
        _ => CaptureCheck::Sound,
    })
    .unwrap();

    let removed: Vec<_> = (check.removed.iter())
        .map(|row| (row.store, row.id.as_str(), row.name.as_str()))
        .collect();
    assert_eq!(
        removed,
        [
            (CaptureStore::Downloads, "cloud", "cloud"),
            (CaptureStore::User, "bad-raw", "Crunch"),
        ]
    );
    assert_eq!(check.unchecked, ["nc2: no NC2 key"]);
    assert!(check.held.is_empty());
    let view = db.view();
    assert_eq!(
        view.download_ids(DownloadKind::Capture).unwrap(),
        Vec::<String>::new()
    );
    let kept: Vec<String> = (0..6)
        .map(|n| format!("good{n}"))
        .chain(["nc2".into()])
        .collect();
    assert_eq!(view.product_ids(ProductType::Capture).unwrap(), kept);
    let root = view.root_folder(ProductType::Capture).unwrap().unwrap();
    assert_eq!(view.folder_products(root).unwrap(), kept);
    assert_eq!(
        view.quarantined_captures().unwrap(),
        [
            QuarantinedCapture {
                store: CaptureStore::Downloads,
                id: "cloud".into(),
                name: "cloud".into(),
                reason: "not a JSON object".into(),
            },
            QuarantinedCapture {
                store: CaptureStore::User,
                id: "bad-raw".into(),
                name: "Crunch".into(),
                reason: "V1 runtime data".into(),
            },
        ]
    );
}

/// A user capture with its user-root link and bytes, for the bound and restore tests.
fn capture_rows(db: &mut StockDb, ids: &[&str]) {
    db.transaction(|write| {
        let (root, _) = write.boot_maintenance()?;
        for id in ids {
            write.put_product(&StockProduct {
                id: (*id).into(),
                product_type: ProductType::Capture,
                read_only: false,
                raw_data: Some(id.as_bytes().to_vec()),
                metadata: Some(b"meta".to_vec()),
            })?;
            write.link(root, id)?;
        }
        Ok(())
    })
    .unwrap();
}

/// Review r23 K5 (O): when more than one row fails and they are more than a quarter of the
/// rows judged, a reader or cipher fault is more likely than corruption, so no row moves.
#[test]
fn a_check_where_too_many_captures_fail_moves_none() {
    let media = Media::new();
    let mut db = StockDb::create(&media.0.join("zenui.db"), DbLimits::default()).unwrap();
    capture_rows(&mut db, &["a", "b", "c"]);
    let check = quarantine_corrupt_captures(&mut db, |raw, _| match raw {
        b"c" => CaptureCheck::Sound,
        _ => CaptureCheck::BadMetadata("not a JSON object".into()),
    })
    .unwrap();
    assert!(check.removed.is_empty());
    let held: Vec<_> = check.held.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(held, ["a", "b"]);
    let view = db.view();
    assert_eq!(
        view.product_ids(ProductType::Capture).unwrap(),
        ["a", "b", "c"]
    );
    assert!(view.quarantined_captures().unwrap().is_empty());
}

/// Review r25 Q7 (O): when the only row judged fails, a reader gap is as likely as
/// corruption, so it stays in the library (held; the caller checks again later).
#[test]
fn a_single_failed_capture_is_held() {
    let media = Media::new();
    let mut db = StockDb::create(&media.0.join("zenui.db"), DbLimits::default()).unwrap();
    capture_rows(&mut db, &["only"]);
    let check =
        quarantine_corrupt_captures(&mut db, |_, _| CaptureCheck::BadMetadata("gap".into()))
            .unwrap();
    assert!(check.removed.is_empty());
    assert_eq!(check.held.len(), 1);
    assert_eq!(
        db.view().product_ids(ProductType::Capture).unwrap(),
        ["only"]
    );
}

/// Review r23 K5 (O): a quarantined capture returns to its store with its bytes and its
/// user-root link, and leaves the quarantine table. A store that holds the id again refuses.
#[test]
fn a_quarantined_capture_returns_to_its_store() {
    let media = Media::new();
    let mut db = StockDb::create(&media.0.join("zenui.db"), DbLimits::default()).unwrap();
    capture_rows(&mut db, &["bad", "good"]);
    let quarantine = |db: &mut StockDb| {
        quarantine_corrupt_captures(db, |raw, _| match raw {
            b"good" => CaptureCheck::Sound,
            _ => CaptureCheck::BadMetadata("synthetic".into()),
        })
        .unwrap()
    };
    assert_eq!(quarantine(&mut db).removed.len(), 1);
    let original = StockProduct {
        id: "bad".into(),
        product_type: ProductType::Capture,
        read_only: false,
        raw_data: Some(b"bad".to_vec()),
        metadata: Some(b"meta".to_vec()),
    };

    restore_quarantined_capture(&mut db, CaptureStore::User, "bad").unwrap();
    let view = db.view();
    assert_eq!(view.product("bad").unwrap(), Some(original));
    let root = view.root_folder(ProductType::Capture).unwrap().unwrap();
    assert_eq!(view.folder_products(root).unwrap(), ["good", "bad"]);
    assert!(view.quarantined_captures().unwrap().is_empty());
    assert!(restore_quarantined_capture(&mut db, CaptureStore::User, "bad").is_err());

    quarantine(&mut db);
    capture_rows(&mut db, &["bad"]);
    assert!(matches!(
        restore_quarantined_capture(&mut db, CaptureStore::User, "bad"),
        Err(crate::Error::Storage(tetrad_storage::Error::Conflict(_)))
    ));
}

fn cloud_product(kind: &str, metadata: serde_json::Value) -> CloudProduct {
    CloudProduct::from_product(&serde_json::json!({
        "id": "cloud-1",
        "name": "Crunch",
        "type": kind,
        "author_username": "someone",
        "creator_version": "4.1.0",
        "created": "1970-01-02T00:00:01Z",
        "metadata": metadata,
        "tags": ["lead"],
    }))
    .unwrap()
}

/// `0x7bb860` → `0x7eced4` / `0x7ee62c(record, 1)` → `0x7eb270`: a V1 capture download in the
/// seed-only cipher becomes a Downloads row in the device cipher (id = cloud id, hash = the
/// payload's member name) that the V1 loader reads; an NC2 download keeps its key and is
/// written again as a bare ObfuscatedGraph; a payload in the wrong cipher is corrupt.
#[test]
fn a_capture_download_becomes_a_device_cipher_row() {
    let global = DeviceCipher::caller_secret(b"synthetic seed-only cipher");
    let local = DeviceCipher::caller_secret(b"synthetic seed-and-serial cipher");
    let nc2 = DeviceCipher::caller_secret(b"synthetic dsp vector");
    let keys = DownloadKeys {
        global: &global,
        local: &local,
        nc2: Some(&nc2),
    };
    let params = params_block();
    let data = tetrad_capture::v1::RuntimeData::parse(&params, Default::default()).unwrap();
    let record = sealed(&global, &conversion::capture_record(&data));
    let payload = ProductPayload::Capture {
        id: "h1".into(),
        record,
        metadata: b"{}".to_vec(),
    };
    let metadata = serde_json::json!({"version": "1", "device_type": "amp_head", "gain_type": 1});
    let product = cloud_product("neural_capture", metadata);
    let (kind, row) = download_row(&product, &payload, Some(&keys)).unwrap();
    assert_eq!(kind, DownloadKind::Capture);
    assert_eq!(
        (row.id.as_str(), row.hash.as_deref()),
        ("cloud-1", Some("h1"))
    );
    let mut raw = row.raw_data.unwrap();
    local.apply(&mut raw);
    let mut expected = params.clone();
    expected[0x2810..0x2814].fill(0);
    assert_eq!(
        tetrad_capture::stored::v1_runtime_data(&raw).unwrap(),
        expected
    );
    let mut json = row.metadata.unwrap();
    local.apply(&mut json);
    let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
    for (key, value) in [
        ("hash", "h1"),
        ("cloudUUID", "cloud-1"),
        ("path", "h1.cns"),
        ("deviceType", "amp_head"),
        ("instrumentType", "instrument_not_set"),
        ("encryption", "Local"),
        ("version", "1"),
        ("parametersBlockLength", "10340"),
        ("date", "1970-01-02T00:00:01Z"),
        ("authorName", "someone"),
        ("corosVersion", "4.1.0"),
    ] {
        assert_eq!(json[key], value, "{key}");
    }
    assert_eq!(json["gainType"], 1);

    let wrong = DownloadKeys {
        global: &local,
        ..keys
    };
    assert!(matches!(
        download_row(&product, &payload, Some(&wrong)),
        Err(crate::Error::CorruptDownload { .. })
    ));

    let graph = [
        &[
            0x0a, 0x02, 0x10, 0x01, 0x12, 0x01, b'm', 0x1d, 7, 0, 0, 0, 0x22, 0x02,
        ][..],
        &[0xaa, 0xbb],
    ]
    .concat();
    let payload = ProductPayload::Capture {
        id: "h2".into(),
        record: sealed(&nc2, &graph),
        metadata: Vec::new(),
    };
    let product = cloud_product("neural_capture", serde_json::json!({"version": "2"}));
    let (_, row) = download_row(&product, &payload, Some(&keys)).unwrap();
    let mut raw = row.raw_data.unwrap();
    nc2.apply(&mut raw);
    let file = tetrad_capture::v2::Capture2File::parse(&raw).unwrap();
    assert_eq!((file.identity, file.data), (7, &[0xaa, 0xbb][..]));
    assert!(
        file.metadata.is_empty(),
        "0x6f4480 writes no metadata strings"
    );
}

/// `0x7ee62c` case 0: a preset download's sidecar has the record fields, then every key of
/// the cloud `metadata` (it wins), then `tags`; the rawdata is the payload once it parses.
#[test]
fn a_preset_download_gets_the_stock_sidecar() {
    let preset = vec![0x12, 3, b'o', b'l', b'd'];
    let metadata = serde_json::json!({"instrument_type": "bass", "extra": 3});
    let product = cloud_product("preset", metadata);
    let (kind, row) =
        download_row(&product, &ProductPayload::Preset(preset.clone()), None).unwrap();
    assert_eq!(kind, DownloadKind::Preset);
    assert_eq!((row.id.as_str(), row.hash), ("cloud-1", None));
    assert_eq!(row.raw_data, Some(preset));
    let sidecar = crate::sidecar::Sidecar::parse(&row.metadata.unwrap()).unwrap();
    for (key, value) in [
        ("name", "Crunch"),
        ("author", "someone"),
        ("cloudId", "cloud-1"),
        ("corosVersion", "4.1.0"),
        ("instrument_type", "bass"),
        ("date", "1970-01-02T00:00:01Z"),
    ] {
        assert_eq!(sidecar.text(key), Some(value), "{key}");
    }
    assert_eq!(sidecar.fields["extra"], 3);
    assert_eq!(sidecar.texts("tags"), ["lead"]);
}

/// A preset's capture dependency (`0x1d8bac` → `0x82e91c`): the Downloads row's bytes go into
/// the Captures Library keyed by the NC hash and linked to the capture user root, once; a
/// hash the library holds is not written again; a user store that is not local refuses it.
#[test]
fn a_capture_dependency_joins_the_library_root_once() {
    use tetrad_storage::stock::CaptureEncryption;
    let media = Media::new();
    let mut db = StockDb::create(&media.0.join("zenui.db"), DbLimits::default()).unwrap();
    let row = |raw: &[u8]| Download {
        id: "cloud-1".into(),
        hash: Some("h1".into()),
        raw_data: Some(raw.to_vec()),
        metadata: Some(b"meta".to_vec()),
    };
    let encryption = |db: &mut StockDb, flag| {
        (db.transaction(|write| write.set_capture_encryption(CaptureStore::User, flag))).unwrap()
    };
    encryption(&mut db, CaptureEncryption::Global);
    let refused = store_library_capture(&mut db, &row(b"first"));
    assert!(
        matches!(refused, Err(crate::Error::Unsupported(_))),
        "{refused:?}"
    );
    encryption(&mut db, CaptureEncryption::Local);
    let root = (db.transaction(|write| write.boot_maintenance()))
        .unwrap()
        .0;
    assert_eq!(
        store_library_capture(&mut db, &row(b"first")).unwrap(),
        LibraryCapture::Added
    );
    assert_eq!(
        store_library_capture(&mut db, &row(b"second")).unwrap(),
        LibraryCapture::Present
    );
    let view = db.view();
    let stored = view.product("h1").unwrap().unwrap();
    assert_eq!(stored.product_type, ProductType::Capture);
    assert_eq!(
        (stored.raw_data.as_deref(), stored.metadata.as_deref()),
        (Some(&b"first"[..]), Some(&b"meta"[..]))
    );
    assert_eq!(view.folder_products(root).unwrap(), ["h1"]);
    assert!(view.download_ids(DownloadKind::Capture).unwrap().is_empty());
}

/// A user capture row with this name and tags in `cipher` (`0x7ee62c` case 2 keys only).
fn user_capture(cipher: &DeviceCipher, hash: &str, name: &str, tags: &[&str]) -> StockProduct {
    let metadata = serde_json::json!({ "name": name, "tags": tags, "hash": hash });
    StockProduct {
        id: hash.into(),
        product_type: ProductType::Capture,
        read_only: false,
        raw_data: Some(format!("model {hash}").into_bytes()),
        metadata: Some(sealed(cipher, metadata.to_string().as_bytes())),
    }
}

/// re-r34 §2: a cloud capture goes into the NC saving destination as `0x6f8584` sorts it:
/// a new hash is added to the folder [3], a taken name refuses it [4], a hash the folder
/// holds is left alone, the same name and tags only link it [7], another name asks [5/8], a
/// name of another capture refuses it [6], a factory hash is left alone [9]. Only [3]
/// writes a row; nothing else changes a stored row.
#[test]
fn a_cloud_capture_lands_in_the_saving_destination_as_stock_sorts_it() {
    use tetrad_storage::stock::{CaptureEncryption, FolderType, NewFolder};
    let media = Media::new();
    let cipher = DeviceCipher::caller_secret(b"synthetic device cipher");
    let mut db = StockDb::create(&media.0.join("zenui.db"), DbLimits::default()).unwrap();
    let (root, folder) = db
        .transaction(|write| {
            write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
            let root = write.boot_maintenance()?.0;
            write.put_product(&user_capture(&cipher, "kept", "Kept", &["a"]))?;
            write.link(root, "kept")?;
            write.put_product(&user_capture(&cipher, "other", "Taken", &[]))?;
            write.link(root, "other")?;
            let folder = write.create_folder(&NewFolder {
                product_type: ProductType::Capture,
                folder_type: FolderType::User,
                name: "Gig",
                parent: root,
                read_only: false,
            })?;
            Ok((root, folder))
        })
        .unwrap();
    let row = |hash: &str| Download {
        id: format!("cloud-{hash}"),
        hash: Some(hash.into()),
        raw_data: Some(b"cloud model".to_vec()),
        metadata: Some(b"cloud metadata".to_vec()),
    };
    let tags = |list: &[&str]| list.iter().map(|tag| tag.to_string()).collect::<Vec<_>>();
    let mut sort = |hash: &str, name: &str, list: &[&str], folder: i64, factory: bool| {
        store_cloud_capture(
            &mut db,
            &row(hash),
            name,
            &tags(list),
            folder,
            factory,
            &cipher,
        )
        .unwrap()
    };
    assert_eq!(
        sort("new", "Taken", &[], folder, false),
        CloudCapture::NameTaken
    );
    assert_eq!(
        sort("kept", "Kept", &["a"], root, false),
        CloudCapture::Present
    );
    assert_eq!(
        sort("kept", "Taken", &[], folder, false),
        CloudCapture::NameOfOther
    );
    assert_eq!(
        sort("kept", "Renamed", &["a"], folder, false),
        CloudCapture::Ask {
            existing: "Kept".into()
        }
    );
    assert_eq!(
        sort("kept", "Kept", &["b"], folder, false),
        CloudCapture::Ask {
            existing: "Kept".into()
        }
    );
    assert_eq!(
        sort("fac", "Factory", &[], folder, true),
        CloudCapture::Factory
    );
    assert_eq!(
        sort("kept", "Kept", &["a"], folder, false),
        CloudCapture::Linked
    );
    assert_eq!(sort("new", "New", &[], folder, false), CloudCapture::Added);

    let view = db.view();
    let kept = view.product("kept").unwrap().unwrap();
    assert_eq!(
        kept,
        user_capture(&cipher, "kept", "Kept", &["a"]),
        "row unchanged"
    );
    let added = view.product("new").unwrap().unwrap();
    assert_eq!(added.raw_data.as_deref(), Some(&b"cloud model"[..]));
    assert_eq!(view.folder_products(folder).unwrap(), ["kept", "new"]);
    assert!(
        view.folder_products(root)
            .unwrap()
            .contains(&"new".to_owned())
    );
    assert!(view.product("fac").unwrap().is_none());
}

/// A9 (r53): a MOVE or COPY of a Downloads capture (`0x1d9954` → `0x1d94d4`): the target
/// adds it from its holder (a new hash: the row becomes a user capture with its rawdata and
/// metadata; a stored hash: a link; a hash the folder holds: nothing), then the Downloads row
/// goes, in one transaction. A key with no row and a read-only target change nothing.
#[test]
fn a_downloads_capture_moves_into_a_user_folder() {
    use tetrad_storage::stock::{CaptureEncryption, FolderType, NewFolder};
    let media = Media::new();
    let cipher = DeviceCipher::caller_secret(b"synthetic device cipher");
    let mut db = StockDb::create(&media.0.join("zenui.db"), DbLimits::default()).unwrap();
    let row = |hash: &str| Download {
        id: format!("cloud-{hash}"),
        hash: Some(hash.into()),
        raw_data: Some(format!("cloud model {hash}").into_bytes()),
        metadata: Some(b"cloud metadata".to_vec()),
    };
    let (root, folder, factory) = db
        .transaction(|write| {
            write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
            let root = write.boot_maintenance()?.0;
            write.put_product(&user_capture(&cipher, "kept", "Kept", &[]))?;
            write.link(root, "kept")?;
            let new = |name, read_only| NewFolder {
                product_type: ProductType::Capture,
                folder_type: FolderType::User,
                name,
                parent: root,
                read_only,
            };
            let folder = write.create_folder(&new("Gig", false))?;
            let factory = write.create_folder(&new("Locked", true))?;
            for hash in ["new", "kept", "gone"] {
                write.put_download(DownloadKind::Capture, &row(hash))?;
            }
            Ok((root, folder, factory))
        })
        .unwrap();
    let moved = |db: &mut StockDb, key: &str, folder| move_download_capture(db, key, folder);
    assert!(moved(&mut db, "absent", folder).is_err());
    assert!(moved(&mut db, "gone", factory).is_err());
    assert!(
        db.view()
            .download(DownloadKind::Capture, "gone")
            .unwrap()
            .is_some()
    );

    assert_eq!(moved(&mut db, "new", folder).unwrap(), DownloadMove::Stored);
    assert_eq!(
        moved(&mut db, "cloud-kept", folder).unwrap(),
        DownloadMove::Linked
    );
    assert_eq!(moved(&mut db, "gone", root).unwrap(), DownloadMove::Stored);
    let view = db.view();
    let stored = view.product("new").unwrap().unwrap();
    assert_eq!(stored.raw_data.as_deref(), Some(&b"cloud model new"[..]));
    assert_eq!(stored.metadata.as_deref(), Some(&b"cloud metadata"[..]));
    assert_eq!(
        view.product("kept").unwrap().unwrap(),
        user_capture(&cipher, "kept", "Kept", &[]),
        "a held capture keeps its row"
    );
    assert_eq!(view.folder_products(folder).unwrap(), ["new", "kept"]);
    assert!(view.download_ids(DownloadKind::Capture).unwrap().is_empty());
    db.transaction(|write| write.put_download(DownloadKind::Capture, &row("kept")))
        .unwrap();
    assert_eq!(
        moved(&mut db, "kept", folder).unwrap(),
        DownloadMove::Present
    );
    assert!(
        db.view()
            .download_ids(DownloadKind::Capture)
            .unwrap()
            .is_empty()
    );
}

/// re-r34 §2.5: OVERWRITE (`0x82ddfc`) writes only the metadata of the capture with the same
/// hash; its rawdata stays byte-exact and no other row changes. An absent hash and a
/// read-only row are refused with nothing written.
#[test]
fn a_capture_overwrite_changes_metadata_only() {
    use tetrad_storage::stock::CaptureEncryption;
    let media = Media::new();
    let cipher = DeviceCipher::caller_secret(b"synthetic device cipher");
    let mut db = StockDb::create(&media.0.join("zenui.db"), DbLimits::default()).unwrap();
    let locked = StockProduct {
        read_only: true,
        ..user_capture(&cipher, "locked", "Locked", &[])
    };
    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
        write.put_product(&user_capture(&cipher, "kept", "Kept", &["a"]))?;
        write.put_product(&user_capture(&cipher, "other", "Other", &[]))?;
        write.put_product(&locked)
    })
    .unwrap();

    overwrite_capture_metadata(&mut db, "kept", b"cloud metadata").unwrap();
    assert!(overwrite_capture_metadata(&mut db, "absent", b"x").is_err());
    assert!(overwrite_capture_metadata(&mut db, "locked", b"x").is_err());

    let view = db.view();
    let kept = view.product("kept").unwrap().unwrap();
    assert_eq!(kept.raw_data.as_deref(), Some(&b"model kept"[..]));
    assert_eq!(kept.metadata.as_deref(), Some(&b"cloud metadata"[..]));
    assert_eq!(
        view.product("other").unwrap().unwrap(),
        user_capture(&cipher, "other", "Other", &[])
    );
    assert_eq!(view.product("locked").unwrap().unwrap(), locked);
    assert!(view.product("absent").unwrap().is_none());
}

/// re-r34 §1 (`0x7bd384`, made-up secrets): a V1 user capture uploads re-keyed from the device
/// cipher to the seed-only cipher (`0x6ae59c`), an NC2 one as stored (it must load with the
/// DSP `0x27` key); the metadata is plain JSON with the runtime data size. An unknown version
/// is refused. After the reply only the record's `cloudUUID` changes. A Downloads capture
/// (re-r38 §1, the preset walk) is found and re-keyed the same way.
#[test]
fn a_v1_capture_upload_is_rekeyed_to_the_seed_cipher_and_nc2_is_not() {
    use tetrad_storage::stock::CaptureEncryption;
    let local = DeviceCipher::caller_secret(b"synthetic seed-and-serial cipher");
    let global = DeviceCipher::caller_secret(b"synthetic seed-only cipher");
    let nc2 = DeviceCipher::caller_secret(b"synthetic dsp vector");
    let media = Media::new();
    let mut db = StockDb::create(&media.0.join("zenui.db"), DbLimits::default()).unwrap();
    let params = params_block();
    let data = tetrad_capture::v1::RuntimeData::parse(&params, Default::default()).unwrap();
    let v1 = conversion::capture_record(&data);
    let graph = [
        &[0x0a, 0x02, 0x10, 0x01, 0x1d, 7, 0, 0, 0, 0x22, 0x02][..],
        &[0xaa, 0xbb],
    ]
    .concat();
    let row = |id: &str, version: &str, raw: Vec<u8>| StockProduct {
        id: id.into(),
        product_type: ProductType::Capture,
        read_only: false,
        raw_data: Some(raw),
        metadata: Some(sealed(
            &local,
            serde_json::json!({"name": id, "version": version, "cloudUUID": ""})
                .to_string()
                .as_bytes(),
        )),
    };
    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
        write.put_product(&row("one", "1", sealed(&local, &v1)))?;
        write.put_product(&row("two", "2", sealed(&nc2, &graph)))?;
        write.put_product(&row("three", "3", b"x".to_vec()))?;
        write.set_capture_encryption(CaptureStore::Downloads, CaptureEncryption::Local)?;
        let four = row("cloud-4", "1", sealed(&local, &v1));
        let download = Download {
            id: four.id,
            hash: Some("four".into()),
            raw_data: four.raw_data,
            metadata: four.metadata,
        };
        write.put_download(DownloadKind::Capture, &download)
    })
    .unwrap();
    let keys = UploadKeys {
        local: &local,
        global: &global,
        nc2: Some(&nc2),
    };

    let one = capture_upload(&db, "one", &keys).unwrap();
    assert_eq!(one.record, sealed(&global, &v1));
    let json: serde_json::Value = serde_json::from_slice(&one.metadata).unwrap();
    assert_eq!(json["parametersBlockLength"], params.len().to_string());
    assert_eq!(json["name"], "one");
    let two = capture_upload(&db, "two", &keys).unwrap();
    assert_eq!(two.record, sealed(&nc2, &graph));
    let no_vector = UploadKeys { nc2: None, ..keys };
    assert!(capture_upload(&db, "two", &no_vector).is_err());
    assert!(capture_upload(&db, "three", &keys).is_err());
    assert!(capture_upload(&db, "absent", &keys).is_err());
    let four = capture_upload(&db, "four", &keys).unwrap();
    assert_eq!(four.record, sealed(&global, &v1));

    set_capture_cloud_id(&mut db, "one", "cloud-9", &local).unwrap();
    let stored = db.view().product("one").unwrap().unwrap();
    assert_eq!(stored.raw_data, Some(sealed(&local, &v1)));
    let mut metadata = stored.metadata.unwrap();
    local.apply(&mut metadata);
    let json: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
    assert_eq!(
        (json["cloudUUID"].as_str(), json["name"].as_str()),
        (Some("cloud-9"), Some("one"))
    );
}

/// The zen-rs to Tetrad rename of the staged copies: an old copy takes the Tetrad suffix; an
/// old copy beside a Tetrad copy stays and is named for the log, and the Tetrad copy keeps its
/// bytes; a zen-rs temporary file goes; other files stay. A second run changes nothing.
#[test]
fn zen_rs_staged_copies_take_the_tetrad_suffixes() {
    let media = Media::new();
    for (file, bytes) in [
        ("a.cns.zen-local", "old a"),
        ("b.json.zen-converted", "old b"),
        ("c.cns.zen-local", "old c"),
        ("c.cns.tetrad-local", "new c"),
        ("d.cns.zen-local.zen-tmp-1-2", "cut"),
        ("e.cns", "stock"),
    ] {
        fs::write(media.legacy(file), bytes).unwrap();
    }
    let library = StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default()).unwrap();
    for _ in 0..2 {
        assert_eq!(library.migrate_zen_rs_files().unwrap(), ["c.cns.zen-local"]);
        let mut files: Vec<_> = fs::read_dir(media.legacy(""))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let bytes = fs::read_to_string(entry.path()).unwrap();
                (entry.file_name().into_string().unwrap(), bytes)
            })
            .collect();
        files.sort();
        let want = [
            ("a.cns.tetrad-local", "old a"),
            ("b.json.tetrad-converted", "old b"),
            ("c.cns.tetrad-local", "new c"),
            ("c.cns.zen-local", "old c"),
            ("e.cns", "stock"),
        ]
        .map(|(file, bytes)| (file.to_owned(), bytes.to_owned()));
        assert_eq!(files, want);
    }
}
