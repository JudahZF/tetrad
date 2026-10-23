//! A blocks-only extension end to end without Node: scaffold, build, package, check and
//! preview.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use tetrad_extensions::manifest::Manifest;
use tetrad_extensions::package::Package;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tetrad_ext(args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_tetrad-ext"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tetrad-ext {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn blocks_only_extension_builds_packages_and_previews() {
    let tmp =
        TempDir(std::env::temp_dir().join(format!("tetrad-ext-blocks-{}", std::process::id())));
    let _ = fs::remove_dir_all(&tmp.0);
    let path = |p: &str| tmp.0.join(p).to_str().unwrap().to_owned();

    // The scaffold's manifest and block IR are valid.
    tetrad_ext(&[
        "init",
        "--name",
        "Kit",
        "--id",
        "test.kit",
        "--blocks",
        &path("init"),
    ]);
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(tmp.0.join("init/tetrad-extension.json")).unwrap())
            .unwrap();
    manifest.validate().unwrap();
    assert_eq!(manifest.blocks[0].id, "gain");
    let ir = fs::read_to_string(tmp.0.join("init/blocks/gain.qcdsp.json")).unwrap();
    assert_eq!(qcdsp::Block::from_json(&ir).unwrap().version(), 1);

    // A blocks-only extension: no src/index.ts, so no Node.
    fs::create_dir_all(tmp.0.join("ext/blocks")).unwrap();
    fs::write(
        tmp.0.join("ext/tetrad-extension.json"),
        r#"{"id": "test.kit", "name": "Kit", "version": "1.0.0", "api": 2,
            "blocks": [{"id": "gain", "name": "Gain", "category": "utility"}]}"#,
    )
    .unwrap();
    fs::write(tmp.0.join("ext/blocks/gain.qcdsp.json"), ir).unwrap();
    tetrad_ext(&["build", &path("ext")]);
    tetrad_ext(&["package", &path("ext"), "--out", &path("kit.tetradext")]);
    let package = Package::decode(&fs::read(tmp.0.join("kit.tetradext")).unwrap()).unwrap();
    assert!(package.module.is_none());
    assert_eq!(package.blocks.keys().collect::<Vec<_>>(), ["gain"]);

    let check = tetrad_ext(&["check", &path("kit.tetradext")]);
    let check = String::from_utf8(check.stdout).unwrap();
    assert!(
        check.contains("block gain \"Gain\" (utility)") && check.contains("CPU "),
        "{check}"
    );

    // Stereo 16-bit in, mixed to mono, through Gain = 0.5.
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(tmp.0.join("in.wav"), spec).unwrap();
    for (left, right) in [(16384, 0), (-32768, 0), (0, 0)] {
        writer.write_sample::<i16>(left).unwrap();
        writer.write_sample::<i16>(right).unwrap();
    }
    writer.finalize().unwrap();
    let preview = tetrad_ext(&[
        "preview",
        "--package",
        &path("kit.tetradext"),
        "--block",
        "gain",
        "--input",
        &path("in.wav"),
        "--output",
        &path("out.wav"),
        "--set",
        "gain=0.5",
    ]);
    assert!(String::from_utf8_lossy(&preview.stderr).contains("44100 Hz"));
    let mut reader = hound::WavReader::open(tmp.0.join("out.wav")).unwrap();
    assert_eq!(
        (reader.spec().channels, reader.spec().sample_rate),
        (1, 44_100)
    );
    let samples: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(samples, [0.125, -0.25, 0.0]);
}
