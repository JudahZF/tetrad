use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tetrad_preset::{Limits, ParamValue, PresetDocument};

#[test]
fn owned_library_workflow_preserves_opaque_data_and_refuses_unsafe_imports() {
    let fixture = Fixture::new(0.25);
    let database = fixture.root.join("owned.sqlite");
    let metadata = fixture.root.join("metadata");
    fs::write(&metadata, [0, 255, 128]).unwrap();
    let run = |command: &str, arguments: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(["library", command])
            .arg(&database)
            .args(arguments)
            .output()
            .unwrap()
    };
    assert!(run("create", &[]).status.success());
    assert_eq!(run("create", &[]).status.code(), Some(1));
    assert!(
        run(
            "folder",
            &[
                "--id",
                "1",
                "--name",
                "Synthetic",
                "--product-type",
                "42",
                "--folder-type",
                "7"
            ]
        )
        .status
        .success()
    );
    let import_args = [
        "--id",
        "opaque",
        "--product-type",
        "42",
        "--input",
        fixture.input.to_str().unwrap(),
        "--metadata",
        metadata.to_str().unwrap(),
        "--folder",
        "1",
    ];
    assert!(run("import", &import_args).status.success());
    let before = fs::read(&database).unwrap();
    assert_eq!(run("import", &import_args).status.code(), Some(1));
    assert_eq!(fs::read(&database).unwrap(), before);
    let result = run("list", &["--count", "10"]);
    assert!(result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["product_ids"],
        serde_json::json!(["opaque"])
    );
    let result = run("inspect", &["--id", "opaque"]);
    assert!(result.status.success());
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["raw_data_bytes"], fixture.bytes.len());
    assert_eq!(report["metadata_bytes"], 3);
    let output = fixture.root.join("library-export");
    assert!(
        run(
            "export",
            &["--id", "opaque", "--output", output.to_str().unwrap()]
        )
        .status
        .success()
    );
    assert_eq!(fs::read(&output).unwrap(), fixture.bytes);
    assert_eq!(
        run(
            "export",
            &["--id", "opaque", "--output", output.to_str().unwrap()]
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(fs::read(&database).unwrap(), before);
    assert_eq!(fs::read(&fixture.input).unwrap(), fixture.bytes);

    assert_eq!(
        run(
            "import",
            &[
                "--id",
                "rollback",
                "--product-type",
                "42",
                "--input",
                fixture.input.to_str().unwrap(),
                "--folder",
                "999"
            ]
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(fs::read(&database).unwrap(), before);
    assert_eq!(run("inspect", &["--id", "rollback"]).status.code(), Some(1));
    let alias = fixture.root.join("database-hardlink");
    fs::hard_link(&database, &alias).unwrap();
    for input in [&database, &alias] {
        assert_eq!(
            run(
                "import",
                &[
                    "--id",
                    "alias",
                    "--product-type",
                    "42",
                    "--input",
                    input.to_str().unwrap()
                ]
            )
            .status
            .code(),
            Some(1)
        );
        assert_eq!(fs::read(&database).unwrap(), before);
    }
    let library =
        tetrad_storage::Library::open_owned(&database, tetrad_storage::Limits::default(), true)
            .unwrap();
    assert_eq!(
        library
            .folder_products(tetrad_storage::FolderId::try_from(1).unwrap(), 10)
            .unwrap(),
        ["opaque"]
    );
}

#[test]
fn owned_library_rejects_existing_sidecar_aliases_before_sqlite_access() {
    for suffix in ["-journal", "-wal", "-shm"] {
        let fixture = Fixture::new(0.25);
        let database = fixture.root.join("owned.sqlite");
        let library =
            tetrad_storage::Library::create_owned(&database, tetrad_storage::Limits::default())
                .unwrap();
        drop(library);
        let before = fs::read(&database).unwrap();
        let sidecar = fixture.root.join(format!("owned.sqlite{suffix}"));
        let sidecar_bytes = [0, 255, 128, 42];
        fs::write(&sidecar, sidecar_bytes).unwrap();
        let mut paths = vec![(database.clone(), sidecar.clone())];
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let file_alias = fixture.root.join("source-symlink");
            let hardlink = fixture.root.join("source-hardlink");
            let directory_alias = fixture.root.join("directory-alias");
            symlink(&sidecar, &file_alias).unwrap();
            fs::hard_link(&sidecar, &hardlink).unwrap();
            symlink(&fixture.root, &directory_alias).unwrap();
            paths.extend([
                (database.clone(), file_alias),
                (database.clone(), hardlink),
                (
                    database.clone(),
                    directory_alias.join(format!("owned.sqlite{suffix}")),
                ),
                (directory_alias.join("owned.sqlite"), sidecar.clone()),
            ]);
        }
        for (database_path, selected) in paths {
            for as_metadata in [false, true] {
                let mut command = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"));
                command
                    .args(["library", "import"])
                    .arg(&database_path)
                    .args(["--id", "refused", "--product-type", "42", "--input"])
                    .arg(if as_metadata {
                        &fixture.input
                    } else {
                        &selected
                    });
                if as_metadata {
                    command.arg("--metadata").arg(&selected);
                }
                let result = command.output().unwrap();
                assert_eq!(result.status.code(), Some(1));
                assert!(result.stdout.is_empty());
                let error: Value = serde_json::from_slice(&result.stderr).unwrap();
                assert!(error["error"].as_str().unwrap().contains("SQLite sidecar"));
                assert_eq!(fs::read(&database).unwrap(), before);
                assert_eq!(fs::read(&sidecar).unwrap(), sidecar_bytes);
                assert_eq!(fs::read(&selected).unwrap(), sidecar_bytes);
                assert_eq!(fs::read(&fixture.input).unwrap(), fixture.bytes);
            }
            let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
                .args(["library", "export"])
                .arg(&database_path)
                .args(["--id", "absent", "--output"])
                .arg(&selected)
                .output()
                .unwrap();
            assert_eq!(result.status.code(), Some(1));
            assert!(result.stdout.is_empty());
            let error: Value = serde_json::from_slice(&result.stderr).unwrap();
            assert!(error["error"].as_str().unwrap().contains("SQLite sidecar"));
            assert_eq!(fs::read(&database).unwrap(), before);
            assert_eq!(fs::read(&sidecar).unwrap(), sidecar_bytes);
        }
    }
}

#[test]
fn owned_library_export_rejects_absent_sidecars_and_parent_aliases() {
    let fixture = Fixture::new(0.25);
    let database = fixture.root.join("owned.sqlite");
    let mut library =
        tetrad_storage::Library::create_owned(&database, tetrad_storage::Limits::default())
            .unwrap();
    let product = tetrad_storage::Product {
        id: "opaque".into(),
        product_type: 42,
        read_only: false,
        data: tetrad_storage::ProductData {
            raw_data: Some(fixture.bytes.clone()),
            metadata: None,
        },
    };
    library
        .apply(&[tetrad_storage::Mutation::InsertProduct(&product)])
        .unwrap();
    drop(library);
    let before = fs::read(&database).unwrap();
    #[cfg(unix)]
    let directory_alias = {
        let alias = fixture.root.join("directory-alias");
        std::os::unix::fs::symlink(&fixture.root, &alias).unwrap();
        alias
    };
    for suffix in ["-journal", "-wal", "-shm"] {
        let sidecar_name = format!("owned.sqlite{suffix}");
        let sidecar = fixture.root.join(&sidecar_name);
        let mut paths = vec![(database.clone(), sidecar.clone())];
        paths.push((database.clone(), PathBuf::from(&sidecar_name)));
        #[cfg(unix)]
        paths.extend([
            (database.clone(), directory_alias.join(&sidecar_name)),
            (directory_alias.join("owned.sqlite"), sidecar.clone()),
        ]);
        for (database_path, output) in paths {
            let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
                .current_dir(&fixture.root)
                .args(["library", "export"])
                .arg(database_path)
                .args(["--id", "opaque", "--output"])
                .arg(&output)
                .output()
                .unwrap();
            assert_eq!(result.status.code(), Some(1));
            assert!(result.stdout.is_empty());
            let error: Value = serde_json::from_slice(&result.stderr).unwrap();
            assert!(error["error"].as_str().unwrap().contains("SQLite sidecar"));
            assert!(!sidecar.exists());
            assert_eq!(fs::read(&database).unwrap(), before);
        }
    }
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["library", "list"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(result.status.success());
    let library =
        tetrad_storage::Library::open_owned(&database, tetrad_storage::Limits::default(), false)
            .unwrap();
    assert_eq!(library.product("opaque").unwrap(), Some(product));
    assert_eq!(fs::read(&database).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn owned_library_export_never_activates_a_dangling_sidecar_link() {
    use std::os::unix::fs::symlink;
    for suffix in ["-journal", "-wal", "-shm"] {
        let fixture = Fixture::new(0.25);
        let database = fixture.root.join("owned.sqlite");
        let mut library =
            tetrad_storage::Library::create_owned(&database, tetrad_storage::Limits::default())
                .unwrap();
        let bytes: Vec<_> = (0..2073).map(|value| (value % 256) as u8).collect();
        let product = tetrad_storage::Product {
            id: "opaque".into(),
            product_type: 42,
            read_only: false,
            data: tetrad_storage::ProductData {
                raw_data: Some(bytes.clone()),
                metadata: None,
            },
        };
        library
            .apply(&[tetrad_storage::Mutation::InsertProduct(&product)])
            .unwrap();
        drop(library);
        let before = fs::read(&database).unwrap();
        let output = fixture.root.join("new-export");
        let chain = fixture.root.join("chain");
        let sidecar = fixture.root.join(format!("owned.sqlite{suffix}"));
        symlink("new-export", &chain).unwrap();
        symlink("chain", &sidecar).unwrap();
        let directory_alias = fixture.root.join("directory-alias");
        symlink(&fixture.root, &directory_alias).unwrap();
        for selected in [&output, &directory_alias.join("new-export")] {
            let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
                .args(["library", "export"])
                .arg(&database)
                .args(["--id", "opaque", "--output"])
                .arg(selected)
                .output()
                .unwrap();
            assert_eq!(result.status.code(), Some(1));
            assert!(result.stdout.is_empty());
            let error: Value = serde_json::from_slice(&result.stderr).unwrap();
            assert!(error["error"].as_str().unwrap().contains("SQLite sidecar"));
            assert!(!output.exists());
            assert_eq!(fs::read(&database).unwrap(), before);
            assert_eq!(fs::read_link(&sidecar).unwrap(), PathBuf::from("chain"));
            assert_eq!(fs::read_link(&chain).unwrap(), PathBuf::from("new-export"));
        }
        fs::rename(&sidecar, fixture.root.join("preserved-sidecar-link")).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(["library", "export"])
            .arg(&database)
            .args(["--id", "opaque", "--output"])
            .arg(&output)
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(fs::read(&output).unwrap(), bytes);
        let library =
            tetrad_storage::Library::open_owned(&database, tetrad_storage::Limits::default(), true)
                .unwrap();
        assert_eq!(library.product("opaque").unwrap(), Some(product));
        assert_eq!(fs::read(&database).unwrap(), before);
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_write_paths_report_success_without_changing_native_filenames() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new(0.25);
    let database = fixture
        .root
        .join(OsString::from_vec(b"owned-\xff.sqlite".to_vec()));
    let check = |result: std::process::Output, field: &str, path: &std::path::Path| {
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stderr.is_empty());
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(
            report[field].as_str(),
            Some(path.to_string_lossy().as_ref())
        );
    };
    check(
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(["library", "create"])
            .arg(&database)
            .output()
            .unwrap(),
        "database",
        &database,
    );
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["library", "import"])
        .arg(&database)
        .args(["--id", "opaque", "--product-type", "42", "--input"])
        .arg(&fixture.input)
        .output()
        .unwrap();
    assert!(result.status.success());
    let output = fixture
        .root
        .join(OsString::from_vec(b"product-\xff.bin".to_vec()));
    check(
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(["library", "export"])
            .arg(&database)
            .args(["--id", "opaque", "--output"])
            .arg(&output)
            .output()
            .unwrap(),
        "output",
        &output,
    );
    assert_eq!(fs::read(&output).unwrap(), fixture.bytes);
    let output = fixture
        .root
        .join(OsString::from_vec(b"gain-\xff.isa".to_vec()));
    check(
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(["sharc", "gain", "--output"])
            .arg(&output)
            .output()
            .unwrap(),
        "output",
        &output,
    );
    assert_eq!(fs::read(&output).unwrap().len(), 54);
    let output = fixture
        .root
        .join(OsString::from_vec(b"roundtrip-\xff.pb".to_vec()));
    check(
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(["preset", "roundtrip"])
            .arg(&fixture.input)
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap(),
        "output",
        &output,
    );
    assert_eq!(fs::read(&output).unwrap(), fixture.bytes);
    let output = fixture
        .root
        .join(OsString::from_vec(b"edit-\xff.pb".to_vec()));
    check(
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(["preset", "edit"])
            .arg(&fixture.input)
            .args(["--name", "New", "--output"])
            .arg(&output)
            .output()
            .unwrap(),
        "output",
        &output,
    );
    let mut expected = PresetDocument::parse(&fixture.bytes, Limits::default()).unwrap();
    expected.rename("New").unwrap();
    assert_eq!(fs::read(&output).unwrap(), expected.to_bytes());
    assert_eq!(fs::read(&fixture.input).unwrap(), fixture.bytes);
}

#[cfg(unix)]
#[test]
fn sqlite_special_names_are_persistent_literal_owned_files() {
    for name in [":memory:", "file:synthetic?mode=memory&cache=shared"] {
        let fixture = Fixture::new(0.25);
        let run = |command: &str, args: &[&str]| {
            Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
                .current_dir(&fixture.root)
                .args(["library", command, name])
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run("create", &[]).status.success());
        let path = fixture.root.join(name);
        assert!(fs::metadata(&path).unwrap().len() >= 100);
        assert!(run("list", &[]).status.success());
        assert!(
            run(
                "import",
                &[
                    "--id",
                    "opaque",
                    "--product-type",
                    "42",
                    "--input",
                    fixture.input.to_str().unwrap()
                ]
            )
            .status
            .success()
        );
        let before = fs::read(&path).unwrap();
        let result = run("inspect", &["--id", "opaque"]);
        assert!(result.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&result.stdout).unwrap()["raw_data_bytes"],
            fixture.bytes.len()
        );
        assert_eq!(run("create", &[]).status.code(), Some(1));
        assert_eq!(fs::read(&path).unwrap(), before);
        let library =
            tetrad_storage::Library::open_owned(&path, tetrad_storage::Limits::default(), true)
                .unwrap();
        assert_eq!(
            library.product("opaque").unwrap().unwrap().data.raw_data,
            Some(fixture.bytes.clone())
        );
    }
}

#[test]
fn owned_sharc_gain_exports_exact_words_and_never_overwrites() {
    let fixture = Fixture::new(0.25);
    let output = fixture.root.join("gain.isa");
    let run = |origin: &str, output: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(["sharc", "gain", "--origin", origin, "--output"])
            .arg(output)
            .output()
            .unwrap()
    };
    let result = run("0x12000", &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["origin_pc_words"], 0x12000);
    assert_eq!(report["word_count"], 9);
    assert_eq!(report["bootable_image"], false);
    assert_eq!(report["loaded"], false);
    let words = [
        0x0f2000000001_u64,
        0x013e00021280,
        0x070000000006,
        0x503e00000000,
        0x013e00130004,
        0x523e80000000,
        0x013e0002a220,
        0x072000fffffc,
        0x0a3e00000000,
    ];
    let expected: Vec<_> = words
        .into_iter()
        .flat_map(|word| word.to_le_bytes()[..6].to_vec())
        .collect();
    assert_eq!(fs::read(&output).unwrap(), expected);
    assert_eq!(run("0", &output).status.code(), Some(1));
    assert_eq!(fs::read(&output).unwrap(), expected);
    for origin in ["0x1000000", "0xfffff8"] {
        let invalid_output = fixture.root.join(format!("bad-{origin}"));
        assert_eq!(run(origin, &invalid_output).status.code(), Some(1));
        assert!(!invalid_output.exists());
    }
}

#[test]
fn decoded_v1_inspection_separates_values_from_structure_and_preserves_source() {
    let fixture = Fixture::new(0.25);
    let path = fixture.root.join("v1-runtime");
    let mut bytes = vec![0_u8; 0x2818];
    for (offset, word) in [
        (0, 48_000_f32.to_bits()),
        (4, 512),
        (8, 2048),
        (0x280c, 1_f32.to_bits()),
        (0x2810, 1_f32.to_bits()),
        (0x2814, 3),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
    }
    for size in [1_u32, 2, 2, 1] {
        bytes.extend_from_slice(&size.to_le_bytes());
    }
    bytes.extend_from_slice(&[0; 13 * 4 + 8]);
    assert_eq!(bytes.len(), 0x2864);
    fs::write(&path, &bytes).unwrap();
    let run = |validate: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"));
        command.args(["capture", "v1"]).arg(&path);
        if validate {
            command.arg("--validate");
        }
        command.output().unwrap()
    };
    let result = run(true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["record_bytes"], bytes.len());
    assert_eq!(report["layer_sizes"], serde_json::json!([1, 2, 2, 1]));
    assert_eq!(report["validation_passed"], true);
    assert_eq!(report["inference_performed"], false);
    assert_eq!(fs::read(&path).unwrap(), bytes);

    bytes[0x280c..0x2810].copy_from_slice(&0x7fc0_1234_u32.to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    for validate in [false, true] {
        let result = run(validate);
        assert_eq!(result.status.code(), Some(if validate { 2 } else { 0 }));
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["validation_passed"], false);
        assert_eq!(report["constant_gain_bits"][0], 0x7fc0_1234_u32);
        assert!(report["constant_gains"][0].is_null());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    for malformed in [
        &bytes[..bytes.len() - 1],
        &[bytes.as_slice(), &[0]].concat(),
    ] {
        fs::write(&path, malformed).unwrap();
        let result = run(false);
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
    }
}

#[test]
fn loader_dsp_and_capture_inspection_are_bounded_read_only_operations() {
    use tetrad_dsp_protocol::{DeploymentTarget, DspCore, ModuleSlot, packet::ParameterChange};
    let fixture = Fixture::new(0.25);
    let dsp_path = fixture.root.join("packet");
    let packet = ParameterChange::from_template(
        [0xab; 20],
        DeploymentTarget {
            core: DspCore::Core1,
            slot: ModuleSlot::try_from(4).unwrap(),
        },
        7,
        f32::from_bits(0x7fc0_1234),
    );
    fs::write(&dsp_path, packet.as_bytes()).unwrap();
    let ldr_path = fixture.root.join("ldr");
    let mut header = [0_u8; 16];
    header[3] = 0xac;
    header[4..8].copy_from_slice(&0x100_u32.to_le_bytes());
    header[8..12].copy_from_slice(&2_u32.to_le_bytes());
    header[2] = header.iter().fold(0, |xor, byte| xor ^ byte);
    let ldr: Vec<_> = header.into_iter().chain([1, 2]).collect();
    fs::write(&ldr_path, &ldr).unwrap();
    let capture_path = fixture.root.join("capture");
    let mut capture = vec![0_u8; 104];
    for (offset, value) in [(0, 4_u32), (4, 4), (32, 4), (44, 1), (60, 2)] {
        capture[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fs::write(&capture_path, &capture).unwrap();
    for (args, path, status_field, expected) in [
        (
            ["dsp", "parameter"],
            &dsp_path,
            "transmitted",
            Value::Bool(false),
        ),
        (["ldr", "inspect"], &ldr_path, "loaded", Value::Bool(false)),
        (
            ["capture", "affine"],
            &capture_path,
            "inference_performed",
            Value::Bool(false),
        ),
    ] {
        let before = fs::read(path).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(args)
            .arg(path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report[status_field], expected);
        if args[0] == "dsp" {
            assert_eq!(report["value_bits"], 0x7fc0_1234_u32);
            assert!(report["value"].is_null());
        }
        assert_eq!(fs::read(path).unwrap(), before);
        fs::write(path, &before[..1]).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(args)
            .arg(path)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
    }
}

#[test]
fn control_inspection_reassembles_saved_bodies_without_performing_actions() {
    let fixture = Fixture::new(0.25);
    let input = fixture.root.join("reports");
    let envelope = tetrad_control::Envelope {
        message_type: 13,
        transform: 1,
        decompress: 0,
        opaque: [7, 8],
        payload: vec![42; 247],
    };
    let reports = envelope.reports(tetrad_control::Limits::default()).unwrap();
    let bytes: Vec<_> = reports.iter().flatten().copied().collect();
    fs::write(&input, &bytes).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["control", "inspect"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(result.status.success());
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["reports"], 3);
    assert_eq!(report["messages"][0]["payload_bytes"], 247);
    assert!(report["messages"][0]["payload_processing_error"].is_string());
    assert_eq!(report["actions_performed"], false);
    assert_eq!(fs::read(&input).unwrap(), bytes);
    fs::write(&input, reports[0]).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["control", "inspect"])
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
}

#[test]
fn blob_validation_rejects_out_of_section_code_relocations() {
    use qc_format::blob::{LinkRange, ModuleBlob, Section, SectionBytes, SectionWords};
    let fixture = Fixture::new(0.25);
    let input = fixture.root.join("blob");
    let blob = ModuleBlob {
        contents: vec![SectionBytes {
            section: Section::Code,
            bytes: vec![0, 0],
        }],
        relocations: vec![SectionWords {
            section: Section::Code,
            words: vec![11, u32::MAX, Section::Code.base(), 0],
        }],
        ranges: vec![LinkRange::for_contents(Section::Code, 2).unwrap()],
        entry: Section::Code.base(),
    };
    fs::write(&input, blob.to_bytes().unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["blob", "validate"])
        .arg(input)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["roundtrip_equal"], true);
    assert_eq!(report["semantic_valid"], false);
    assert!(report["semantic_error"].is_string());
}

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    input: PathBuf,
    bytes: Vec<u8>,
}

impl Fixture {
    fn new(value: f32) -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let input = root.join("input");
        let mut bytes = vec![0x12, 8];
        bytes.extend_from_slice(b"original");
        bytes.extend_from_slice(&[0x7a, 1, b'A']); // Scene label.
        // Chain.models[0].params[0].param_values[0].float_value.
        bytes.extend_from_slice(&[0x5a, 11, 0x2a, 9, 0x12, 7, 0x2a, 5, 0x15]);
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes.extend_from_slice(&[0xa0, 0x02, 0, 0xa0, 0x06, 7]); // Explicit false and unknown field.
        fs::write(&input, &bytes).unwrap();
        Self { root, input, bytes }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn cli_roundtrip_and_edit_preserve_source_and_refuse_existing_targets() {
    let fixture = Fixture::new(0.25);
    let output = fixture.root.join("export");
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["preset", "roundtrip"])
        .arg(&fixture.input)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(&output).unwrap(), fixture.bytes);
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["roundtrip_equal"], true);
    for target in [&fixture.input, &output] {
        let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(["preset", "edit"])
            .arg(&fixture.input)
            .arg("--output")
            .arg(target)
            .args(["--name", "replacement"])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(serde_json::from_slice::<Value>(&result.stderr).unwrap()["error"].is_string());
    }
    assert_eq!(fs::read(&fixture.input).unwrap(), fixture.bytes);
    assert_eq!(fs::read(output).unwrap(), fixture.bytes);
}

#[test]
fn cli_routes_each_edit_to_the_requested_wire_field() {
    let fixture = Fixture::new(0.25);
    for (index, args) in [
        vec!["--name", "new"],
        vec!["--scene-label-index", "0", "--scene-label", "B"],
        vec![
            "--chain",
            "0",
            "--model",
            "0",
            "--parameter",
            "0",
            "--scene",
            "0",
            "--float",
            "-0.5",
        ],
        vec![
            "--chain",
            "0",
            "--model",
            "0",
            "--parameter",
            "0",
            "--scene",
            "0",
            "--integer",
            "-7",
        ],
        vec![
            "--chain",
            "0",
            "--model",
            "0",
            "--parameter",
            "0",
            "--scene",
            "0",
            "--string",
            "text",
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let output = fixture.root.join(format!("edit-{index}"));
        let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(["preset", "edit"])
            .arg(&fixture.input)
            .arg("--output")
            .arg(&output)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let mut expected = PresetDocument::parse(&fixture.bytes, Limits::default()).unwrap();
        match index {
            0 => expected.rename("new").unwrap(),
            1 => expected.set_scene_label(0, "B").unwrap(),
            2 => expected
                .set_parameter(0, 0, 0, 0, ParamValue::Float(-0.5))
                .unwrap(),
            3 => expected
                .set_parameter(0, 0, 0, 0, ParamValue::Int(-7))
                .unwrap(),
            4 => expected
                .set_parameter(0, 0, 0, 0, ParamValue::String("text".into()))
                .unwrap(),
            _ => unreachable!(),
        }
        assert_eq!(fs::read(output).unwrap(), expected.to_bytes());
    }
    assert_eq!(fs::read(&fixture.input).unwrap(), fixture.bytes);
}

#[test]
fn incomplete_conflicting_and_nonfinite_edits_create_no_output() {
    let fixture = Fixture::new(0.25);
    let output = fixture.root.join("export");
    for args in [
        vec![],
        vec![
            "--name",
            "X",
            "--scene-label",
            "Y",
            "--scene-label-index",
            "0",
        ],
        vec!["--scene-label", "Y"],
        vec!["--scene-label-index", "0"],
        vec!["--float", "0.5"],
        vec!["--name", "X", "--chain", "0"],
        vec![
            "--chain",
            "0",
            "--model",
            "0",
            "--parameter",
            "0",
            "--scene",
            "0",
            "--float",
            "NaN",
        ],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .args(["preset", "edit"])
            .arg(&fixture.input)
            .arg("--output")
            .arg(&output)
            .args(args)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!output.exists());
    }
    assert_eq!(fs::read(&fixture.input).unwrap(), fixture.bytes);
}

#[test]
fn validation_errors_are_distinct_from_lossless_roundtrip() {
    let fixture = Fixture::new(f32::from_bits(0x7fc0_0123));
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["preset", "validate"])
        .arg(&fixture.input)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["validation_passed"], false);
    assert!(
        response["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["severity"] == "error")
    );
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["preset", "roundtrip"])
        .arg(&fixture.input)
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(fs::read(&fixture.input).unwrap(), fixture.bytes);
}

#[test]
fn validation_keeps_diagnostics_when_a_typed_summary_cannot_be_built() {
    let fixture = Fixture::new(0.25);
    fs::write(&fixture.input, [0x10, 0]).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["preset", "validate"])
        .arg(&fixture.input)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stderr.is_empty());
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["validation_passed"], false);
    assert!(response["summary"].is_null());
    assert!(response["summary_error"].is_string());
    assert!(
        response["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "known_wire_type")
    );
}

#[test]
fn missing_inputs_and_requested_blob_checks_never_silently_pass() {
    let fixture = Fixture::new(0.25);
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["preset", "roundtrip"])
        .arg(fixture.root.join("absent"))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let repo = fixture.root.join("models.xml");
    let report = fixture.root.join("report.xml");
    let blobs = fixture.root.join("blobs");
    fs::write(&repo, "<Models><Category id='1' name='Utility'><Model id='2' name='Test' blob='test'/></Category></Models>").unwrap();
    fs::write(&report, "<ModuleReport/>").unwrap();
    fs::create_dir(&blobs).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["catalog", "validate", "--model-repo"])
        .arg(&repo)
        .arg("--module-report")
        .arg(&report)
        .arg("--blobs")
        .arg(&blobs)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["blob_checks_requested"], true);
    assert_eq!(response["blobs"]["missing_referenced"], 1);
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["catalog", "validate", "--model-repo"])
        .arg(&repo)
        .arg("--module-report")
        .arg(&report)
        .arg("--blobs")
        .arg(fixture.root.join("absent"))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
}
