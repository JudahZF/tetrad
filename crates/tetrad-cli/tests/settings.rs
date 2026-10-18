use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static NEXT: AtomicU64 = AtomicU64::new(0);

const ZENJACK: &str =
    "[General]\nBrightness=7\n\n[Cloud]\nApiToken=synthetic-token-value\nUser=tester\n";
const LICENSES: &str = "[Plugins]\nSynthPro=synthetic-license-blob\n";

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-settings-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .arg("settings")
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

fn find<'a>(entries: &'a [Value], group: &str, key: &str) -> &'a Value {
    entries
        .iter()
        .find(|entry| entry["group"] == group && entry["key"] == key)
        .unwrap_or_else(|| panic!("missing {group}/{key}"))
}

#[test]
fn dump_lists_typed_keys_and_redacts_secrets() {
    let scratch = Scratch::new();
    fs::write(scratch.root.join("ZenjackConfig.ini"), ZENJACK).unwrap();
    fs::write(scratch.root.join("PluginLicenses.ini"), LICENSES).unwrap();
    let dir = scratch.root.to_str().unwrap();

    let output = scratch.cli(&["dump", "--dir", dir]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("synthetic-token-value"));
    assert!(!stdout.contains("synthetic-license-blob"));

    let report: Value = serde_json::from_str(&stdout).unwrap();
    let files = report["files"].as_array().unwrap();
    let zenjack = files
        .iter()
        .find(|file| file["file"] == "ZenjackConfig.ini")
        .unwrap();
    assert_eq!(zenjack["present"], true);
    assert_eq!(zenjack["format_issues"], 0);
    let entries = zenjack["entries"].as_array().unwrap();
    assert_eq!(
        find(entries, "", "Brightness")["value"],
        serde_json::json!({ "type": "string", "value": "7" })
    );
    assert_eq!(
        find(entries, "Cloud", "ApiToken")["value"]["type"],
        "redacted"
    );
    assert_eq!(find(entries, "Cloud", "User")["value"]["value"], "tester");

    let permanent = files
        .iter()
        .find(|file| file["file"] == "PermanentSettings.ini")
        .unwrap();
    assert_eq!(permanent["present"], false);

    let licenses = files
        .iter()
        .find(|file| file["file"] == "PluginLicenses.ini")
        .unwrap();
    assert_eq!(
        find(
            licenses["entries"].as_array().unwrap(),
            "Plugins",
            "SynthPro"
        )["value"]["type"],
        "redacted"
    );
}
