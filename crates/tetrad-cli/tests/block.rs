use std::fs;
use std::process::{Command, Output};

use qc_format::blob::ModuleBlob;
use serde_json::Value;

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .arg("block")
        .args(args)
        .output()
        .unwrap()
}

fn json_report(output: Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Item 4 (`w4-f-wire`): `build` writes a valid plaintext blob for the stock Volume ID once,
/// and `simulate` runs it on the modeled SHARC within its cycle bound.
#[test]
fn volume_builds_once_and_simulates_within_its_bound() {
    let root = std::env::temp_dir().join(format!("tetrad-block-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let output = root.join("volume.blob");
    let path = output.to_str().unwrap();

    let built = json_report(cli(&["build", "volume", "--output", path]));
    assert_eq!(built["model"]["stock_id"], 16_006);
    assert_eq!(built["loaded"], false);
    let bytes = fs::read(&output).unwrap();
    assert_eq!(Some(bytes.len() as u64), built["blob_bytes"].as_u64());
    ModuleBlob::parse(&bytes).unwrap().validate().unwrap();
    assert_eq!(
        cli(&["build", "volume", "--output", path]).status.code(),
        Some(1)
    );
    fs::remove_dir_all(&root).unwrap();

    let simulated = json_report(cli(&["simulate", "volume", "--frames", "64"]));
    assert_eq!(simulated["frames"], 64);
    assert_eq!(simulated["matches_host_twin"], true);
    let bound = simulated["model"]["cycles_per_stereo_block"]
        .as_u64()
        .unwrap();
    let cycles = simulated["maximum_process_cycles"].as_u64().unwrap();
    assert!(cycles > 0 && cycles <= bound);
}
