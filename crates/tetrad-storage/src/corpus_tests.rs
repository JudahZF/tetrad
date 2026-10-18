//! Read-only checks against copies of a device's `/media/p4` and `/opt/neuraldsp` in
//! `$QC_CORPUS_DIR/rootfs` (README, "Corpus tests"). Run with `QC_CORPUS_DIR=<dir>
//! cargo test -p tetrad-storage -- --ignored`. Nothing is written; no corpus bytes
//! are embedded.

use super::media::{MediaFile, MediaRoot, decode_setlist, encode_setlist, read_file};
use super::search_index::{SearchChanges, SearchIndex};
use super::stock::{COROS_4_1_SCHEMA, DbLimits, ProductType, StockDb, ir_product_id};

/// `$QC_CORPUS_DIR/rootfs/<path>`, or `None` with a note when the variable is not set.
fn rootfs(path: &str) -> Option<std::path::PathBuf> {
    let Some(dir) = std::env::var_os("QC_CORPUS_DIR") else {
        eprintln!("skipped: set QC_CORPUS_DIR");
        return None;
    };
    Some(std::path::Path::new(&dir).join("rootfs").join(path))
}

fn media() -> Option<MediaRoot> {
    rootfs("media/p4").map(MediaRoot::new)
}

#[test]
#[ignore = "reads $QC_CORPUS_DIR/rootfs"]
fn corpus_search_files_round_trip_byte_exact() {
    let Some(media) = media() else {
        return;
    };
    let Some(factory) = rootfs("opt/neuraldsp") else {
        return;
    };
    for path in [
        media.file(MediaFile::SearchIndexUser),
        std::path::Path::new(&factory).join("searchIndexFactory"),
    ] {
        let bytes = read_file(&path, 64 << 20).unwrap().unwrap();
        assert_eq!(
            SearchIndex::decode(&bytes, 1 << 20).unwrap().encode(),
            bytes
        );
    }
    let bytes = read_file(&media.file(MediaFile::SearchRecentChangesUser), 1 << 20)
        .unwrap()
        .unwrap();
    assert_eq!(
        SearchChanges::decode(&bytes, 1 << 20).unwrap().encode(),
        bytes
    );
}

#[test]
#[ignore = "reads $QC_CORPUS_DIR/rootfs"]
fn corpus_setlists_round_trip_and_database_opens_read_only() {
    let Some(media) = media() else {
        return;
    };
    for entry in std::fs::read_dir(media.file(MediaFile::Presets)).unwrap() {
        let path = entry.unwrap().path().join("setlist");
        let bytes = read_file(&path, 1 << 20).unwrap().unwrap();
        assert_eq!(
            encode_setlist(&decode_setlist(&bytes).unwrap()).unwrap(),
            bytes
        );
    }
    let db = StockDb::open(&media.file(MediaFile::Database), DbLimits::default(), true).unwrap();
    assert_eq!(db.version(), COROS_4_1_SCHEMA);
    let view = db.view();
    for id in view.product_ids(ProductType::Capture).unwrap() {
        assert!(view.product(&id).unwrap().is_some());
    }
    // Every user IR id is `CIR_` + the hex MD5 bytes of its rawdata.
    let irs = view.product_ids(ProductType::Ir).unwrap();
    for id in &irs {
        let raw = view.product(id).unwrap().unwrap().raw_data.unwrap();
        let md5: [u8; 16] = <md5::Md5 as md5::Digest>::digest(&raw).into();
        assert_eq!(ir_product_id(&md5), *id);
    }
    eprintln!("user IR ids checked: {}", irs.len());
    view.folders().unwrap();
    view.model_presets().unwrap();
    view.default_model_presets().unwrap();
    view.pinned_models().unwrap();
    for model in view.default_param_models().unwrap() {
        view.default_params(&model).unwrap();
        view.default_exp_switch(&model).unwrap();
    }

    let Some(factory) = rootfs("opt/neuraldsp") else {
        return;
    };
    let factory = StockDb::open_factory(
        &std::path::Path::new(&factory).join("factory.db"),
        DbLimits::default(),
    )
    .unwrap();
    let view = factory.view();
    assert!(!view.product_ids(ProductType::Capture).unwrap().is_empty());
    for preset in view.model_presets().unwrap().iter().take(64) {
        view.model_preset_params(preset.id).unwrap();
    }
    assert!(
        view.folders()
            .unwrap()
            .iter()
            .all(|folder| folder.read_only)
    );
}
