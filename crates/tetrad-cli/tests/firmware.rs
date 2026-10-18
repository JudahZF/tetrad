use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Reference graph outputs use unity gain and two frames of latency. Frame
/// index 0 carries the ramp of 1000 * step on channel 0 and 500 * step on channel 1.
const RAMP_SUM_CH0: i64 = 1000 * 528;
const RAMP_SUM_CH1: i64 = 500 * 528;

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-firmware-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .arg("firmware")
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn json_report(output: Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn build_writes_the_resident_ldr_once() {
    let scratch = Scratch::new();
    let output = scratch.path("resident.ldr");
    let path = output.to_str().unwrap();

    let result = json_report(scratch.cli(&["build", "--output", path]));
    assert_eq!(result["loaded"], false);
    assert_eq!(result["hardware_compatibility"], "unverified");
    let written = fs::metadata(&output).unwrap().len();
    assert_eq!(Some(written), result["ldr_bytes"].as_u64());

    let refused = scratch.cli(&["build", "--output", path]);
    assert_eq!(refused.status.code(), Some(1));
}

#[test]
fn simulate_passes_ramps_through_the_reference_graph_after_two_frames() {
    let scratch = Scratch::new();

    let result = json_report(scratch.cli(&["simulate", "--frames", "3"]));
    assert_eq!(result["frames_run"], 3);
    let frames = result["frames"].as_array().unwrap();
    let sums = |index: usize| -> Vec<i64> {
        frames[index]["channel_sums"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_i64().unwrap())
            .collect()
    };

    assert_eq!(sums(0), vec![0; 8]);
    assert_eq!(sums(1), vec![0; 8]);
    assert_eq!(
        sums(2),
        vec![0, 0, RAMP_SUM_CH0, RAMP_SUM_CH0 + RAMP_SUM_CH1, 0, 0, 0, 0]
    );
    assert_eq!(frames[2]["index"], 2);
}

/// Path (b): Volume → Low-High Cut builds to an LDR file and simulates with a
/// script. Bypass with mute, sent before frame 2, reaches the blocks in frame 3
/// and ramps for 30 frames (stock 20 ms mixer); the TDM output lags 2 frames.
#[test]
fn owned_block_chain_builds_and_simulates_a_bypass_script() {
    let scratch = Scratch::new();
    let output = scratch.path("owned.ldr");
    let blocks = ["--block", "volume", "--block", "lohicut"];
    let mut args = vec!["build", "--output", output.to_str().unwrap()];
    args.extend(blocks);
    let built = json_report(scratch.cli(&args));
    assert_eq!(built["blocks"][1]["model"], "lohicut");
    assert_eq!(
        Some(fs::metadata(&output).unwrap().len()),
        built["ldr_bytes"].as_u64()
    );

    let script = scratch.path("script.json");
    fs::write(
        &script,
        r#"[{"parameter": {"frame": 0, "slot": 0, "index": 0, "value": 1.0}},
            {"bypass": {"frame": 2, "slot": 1, "value": 2.0}}]"#,
    )
    .unwrap();
    let mut args = vec![
        "simulate",
        "--frames",
        "40",
        "--script",
        script.to_str().unwrap(),
    ];
    args.extend(blocks);
    let result = json_report(scratch.cli(&args));
    assert_eq!(
        [
            &result["messages"],
            &result["rejected"],
            &result["fault_vector"]
        ],
        [2, 0, 0]
    );
    let peaks: Vec<f64> = result["output_channel_0_peaks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|peak| peak.as_f64().unwrap())
        .collect();
    assert!(peaks[33] > 0.0, "the last ramp frames still pass sound");
    assert!(peaks[35..].iter().all(|peak| *peak == 0.0), "{peaks:?}");
}
