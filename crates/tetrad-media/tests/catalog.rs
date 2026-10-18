use std::path::PathBuf;

use tetrad_media::catalog::{
    CatalogError, CatalogLimits, FactoryIrCatalog, FactoryIrEntity, FactoryIrFile, MicPlacement,
};

const LIMITS: CatalogLimits = CatalogLimits {
    max_bytes: 4 << 20,
    max_entities: 4096,
    max_files_per_entity: 64,
};

fn entity(suffixes: &[&str]) -> FactoryIrEntity {
    FactoryIrEntity {
        tag: "XX".into(),
        cab: "212 Test Cab".into(),
        mic: "Dynamic 57".into(),
        readable_name: "XX 212 Test Cab Dynamic 57".into(),
        files: suffixes
            .iter()
            .map(|suffix| FactoryIrFile {
                path: format!(
                    "/opt/neuraldsp/impulse_responses/XX_212 Test Cab_Dynamic 57_{suffix}.bin"
                ),
                placement: MicPlacement::parse(suffix).unwrap(),
            })
            .collect(),
    }
}

#[test]
fn every_recovered_suffix_form_parses_and_others_are_refused() {
    let grid = MicPlacement::Grid {
        position: 2,
        distance: 5,
    };
    assert_eq!(MicPlacement::parse("C5"), Some(grid));
    assert_eq!(MicPlacement::parse("C 5"), Some(grid));
    assert_eq!(MicPlacement::parse("8"), Some(MicPlacement::Step(8)));
    assert_eq!(MicPlacement::parse("01"), Some(MicPlacement::Step(1)));
    assert_eq!(MicPlacement::parse("FAR"), Some(MicPlacement::Far));
    for bad in ["", "c5", "C  5", "C55", "100", "NEAR", "A-1"] {
        assert_eq!(MicPlacement::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn catalog_round_trips_and_resolves_cab_selectors_and_sparse_grids() {
    let catalog = FactoryIrCatalog::new(vec![entity(&["A 0", "B 1", "C1"]), entity(&["1", "FAR"])]);
    let decoded = FactoryIrCatalog::decode(&catalog.encode(), LIMITS).unwrap();
    assert_eq!(decoded, catalog);
    let found = decoded.by_selector("XX_212 Test Cab_Dynamic 57").unwrap();
    assert_eq!(found.selector(), "XX_212 Test Cab_Dynamic 57");
    // Sparse sets exist in the stock catalog; lookup is by placement, not index.
    assert!(
        found
            .file(MicPlacement::Grid {
                position: 2,
                distance: 1
            })
            .unwrap()
            .path
            .ends_with("_C1.bin")
    );
    assert_eq!(
        found.file(MicPlacement::Grid {
            position: 0,
            distance: 1
        }),
        None
    );
    assert_eq!(decoded.by_selector("XX_212 Test Cab"), None);
}

#[test]
fn catalog_decode_is_strict_and_bounded() {
    let mut foreign = entity(&["A0"]);
    foreign.files[0].path = "/tmp/XX_212 Test Cab_Dynamic 57_A0.bin".into();
    let bytes = FactoryIrCatalog::new(vec![foreign]).encode();
    assert!(matches!(
        FactoryIrCatalog::decode(&bytes, LIMITS),
        Err(CatalogError::ForeignFile { entity: 0, .. })
    ));
    let bytes = FactoryIrCatalog::new(vec![entity(&["A0", "A1"])]).encode();
    let tight = CatalogLimits {
        max_files_per_entity: 1,
        ..LIMITS
    };
    assert_eq!(
        FactoryIrCatalog::decode(&bytes, tight),
        Err(CatalogError::FileLimit {
            entity: 0,
            limit: 1
        })
    );
    assert!(matches!(
        FactoryIrCatalog::decode(&bytes[..bytes.len() - 1], LIMITS),
        Err(CatalogError::Proto(_))
    ));
    // Entity missing `mic` and `readable_name`.
    let partial = [0x0a, 0x08, 0x0a, 0x02, b'X', b'X', 0x12, 0x02, b'C', b'B'];
    assert_eq!(
        FactoryIrCatalog::decode(&partial, LIMITS),
        Err(CatalogError::MissingField {
            entity: 0,
            field: "mic"
        })
    );
}

#[test]
fn rebuild_sorts_case_insensitively_groups_runs_and_drops_stock_rejects() {
    let (catalog, skipped) = FactoryIrCatalog::rebuild([
        "XX_212 Test Cab_Dynamic 57_B0.bin",
        "xx_212 Test Cab_Dynamic 57_A1.BIN",
        "XX_212 Test Cab_Dynamic 57_A0.bin",
        "XX_212 Test Cab_Dynamic 57_A R.bin",
        "XX_212 Test Cab_Ribbon 160_FAR.bin",
        "XX_212 Test Cab_Ribbon 160_1.bin",
        "XX_212 Test Cab_Ribbon 160_1.wav",
        "XX_Two Parts_7.bin",
    ]);
    let names: Vec<_> = catalog
        .entities()
        .iter()
        .map(|entity| (entity.readable_name.as_str(), entity.files.len()))
        .collect();
    // Case-insensitive order puts "xx_…A1" between "XX_…A0" and "XX_…B0",
    // and the differing tag case splits the run into separate entities.
    assert_eq!(
        names,
        [
            ("XX 212 Test Cab Dynamic 57", 1),
            ("xx 212 Test Cab Dynamic 57", 1),
            ("XX 212 Test Cab Dynamic 57", 1),
            ("XX 212 Test Cab Ribbon 160", 2),
        ]
    );
    assert_eq!(skipped, ["XX_Two Parts_7.bin"]);
    let ribbon = catalog.by_selector("XX_212 Test Cab_Ribbon 160").unwrap();
    assert_eq!(ribbon.files[1].placement, MicPlacement::Far);
}

#[test]
#[ignore = "reads the IR catalog and the factory IR names in $QC_CORPUS_DIR/rootfs"]
fn local_stock_catalog_matches_a_rebuild_of_the_factory_directory() {
    let Some(neuraldsp) = std::env::var_os("QC_CORPUS_DIR")
        .map(|dir| PathBuf::from(dir).join("rootfs/opt/neuraldsp"))
    else {
        eprintln!("skipped: set QC_CORPUS_DIR");
        return;
    };
    let path = neuraldsp.join("factory_ir_entities.pb");
    let catalog = FactoryIrCatalog::decode(&std::fs::read(path).unwrap(), LIMITS).unwrap();
    assert_eq!(catalog.entities().len(), 690);
    let files: usize = catalog
        .entities()
        .iter()
        .map(|entity| entity.files.len())
        .sum();
    assert_eq!(files, 11_515);

    let dir = neuraldsp.join("impulse_responses");
    let names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    let (rebuilt, skipped) = FactoryIrCatalog::rebuild(names.iter().map(String::as_str));
    assert!(skipped.is_empty());
    assert_eq!(rebuilt, catalog);
}
