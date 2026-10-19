//! Local stock corpus checks for per-model setup. Run with `--ignored` and `QC_CORPUS_DIR`
//! (docs/development.md, "Corpus tests"); they copy no bytes into the repository.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tetrad_dsp_host::setup::{DriverTables, ElfTables, StockTables, driver_kind, drivers};

/// `$QC_CORPUS_DIR/<path>`, or `None` with a note when the variable is not set.
fn corpus(path: &str) -> Option<PathBuf> {
    let Some(dir) = std::env::var_os("QC_CORPUS_DIR") else {
        eprintln!("skipped: set QC_CORPUS_DIR to read {path}");
        return None;
    };
    Some(PathBuf::from(dir).join(path))
}

/// Every constructor table resolves in the pinned ZenUI with its stock length.
#[test]
#[ignore = "reads $QC_CORPUS_DIR/rootfs/opt/neuraldsp/ZenUI"]
fn constructor_tables_resolve_in_pinned_zenui() {
    let Some(zenui) = corpus("rootfs/opt/neuraldsp/ZenUI") else {
        return;
    };
    let tables = ElfTables::new(std::fs::read(zenui).unwrap()).unwrap();
    let mut count = 0;
    for (kind, model, payload) in drivers::constructor_tables() {
        let bytes = tables.read(payload).unwrap();
        assert_eq!(bytes.len(), payload.len(), "{kind:?} {model}");
        count += 1;
    }
    assert_eq!(count, 21);
}

/// One checked image gives every table a driver reads: the kept payload
/// ranges resolve with their stock lengths (notes, amps and media read at load).
#[test]
#[ignore = "reads $QC_CORPUS_DIR/rootfs/opt/neuraldsp/ZenUI"]
fn driver_tables_load_from_one_checked_image() {
    let Some(zenui) = corpus("rootfs/opt/neuraldsp/ZenUI") else {
        return;
    };
    let tables = DriverTables::load(std::fs::read(zenui).unwrap()).unwrap();
    for payload in drivers::stock_payloads() {
        assert_eq!(tables.read(payload).unwrap().len(), payload.len());
    }
}

/// `(model id, category id)` for every ModelRepo model, by a plain scan.
fn models(repo: PathBuf) -> Vec<(u32, u32)> {
    let xml = std::fs::read_to_string(repo).unwrap();
    let attribute = |tag: &str, name: &str| -> u32 {
        let key = format!(" {name}=\"");
        let start = tag.find(&key).unwrap() + key.len();
        let end = start + tag[start..].find('"').unwrap();
        tag[start..end].parse().unwrap()
    };
    let mut category = 0;
    let mut models = Vec::new();
    for tag in xml.split('<').skip(1) {
        if tag.starts_with("Category ") {
            category = attribute(tag, "id");
        } else if tag.starts_with("Model ") {
            models.push((attribute(tag, "id"), category));
        }
    }
    models
}

/// The factory over the whole repository: counts per driver class match the
/// transcription of VA 0x50eef8 (c-dsp2 record, section 2).
#[test]
#[ignore = "reads $QC_CORPUS_DIR/plain/ModelRepo.xml"]
fn factory_covers_model_repo() {
    let Some(repo) = corpus("plain/ModelRepo.xml") else {
        return;
    };
    let models = models(repo);
    assert_eq!(models.len(), 633);
    let mut counts = BTreeMap::new();
    for (model, category) in models {
        let name =
            driver_kind(model, category).map_or("none".to_owned(), |kind| format!("{kind:?}"));
        *counts.entry(name).or_insert(0) += 1;
    }
    let count = |name: &str| counts.get(name).copied().unwrap_or(0);
    assert_eq!(count("none"), 279);
    assert_eq!(count("CabinetSimulator"), 103);
    assert_eq!(count("CabinetSimulatorStereo"), 97);
    assert_eq!(count("Reverb"), 18);
    assert_eq!(count("TempoModulation"), 16);
    assert_eq!(count("PcomSkews"), 11);
    assert_eq!(counts.values().sum::<usize>(), 633);
}
