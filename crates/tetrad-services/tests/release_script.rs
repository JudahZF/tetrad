//! `scripts/release.sh`, the version and manifest steps of the release workflow, against the
//! contract in docs/releases.md: what units in the field (0.1.0-alpha.9 and alpha.10) take, and
//! how a nightly version sorts.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::process::{Command, Output};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signer, SigningKey};
use semver::Version;
use serde::Deserialize;
use tetrad_services::updater::tetrad::Manifest;

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/release.sh");

fn release(args: &[&str]) -> Output {
    Command::new("sh").arg(SCRIPT).args(args).output().unwrap()
}

fn stdout(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// The manifest schema of 0.1.0-alpha.9 and alpha.10, frozen: units in the field parse with
/// it, and refuse any key that it does not have.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldManifest {
    version: Version,
    files: BTreeMap<String, FieldEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldEntry {
    size: u64,
    sha256: String,
}

/// A stable manifest keeps the schema of the units in the field and verifies for its version;
/// a nightly manifest has `"channel": "nightly"`, which those units refuse.
#[test]
fn only_a_nightly_manifest_has_a_channel() {
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("skipped: no jq");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tetrad-release-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("tetrad"), vec![3_u8; 4321]).unwrap();
    let key = SigningKey::from_bytes(&[7; 32]);
    let public = BASE64.encode(key.verifying_key().as_bytes());
    let manifest = |version: &str, channel: &str| {
        stdout(release(&[
            "manifest",
            version,
            channel,
            dir.to_str().unwrap(),
        ]));
        fs::read(dir.join("tetrad-update.json")).unwrap()
    };

    let stable = manifest("0.1.0", "stable");
    let field: FieldManifest = serde_json::from_slice(&stable).unwrap();
    assert_eq!(field.version, v("0.1.0"));
    assert_eq!(field.files["bin/tetrad"].size, 4321);
    assert_eq!(field.files["bin/tetrad"].sha256.len(), 64);
    let signature = BASE64.encode(key.sign(&stable).to_bytes());
    let verified = Manifest::verify(&stable, signature.as_bytes(), &public, Some(&v("0.1.0")));
    let verified = verified.unwrap();
    for (path, entry) in &verified.files {
        let bytes = fs::read(dir.join(path.rsplit('/').next().unwrap())).unwrap();
        assert_eq!(entry.size, bytes.len() as u64, "{path}");
    }

    let nightly = manifest("0.1.1-nightly.20261112.317+61697fa", "nightly");
    let value: serde_json::Value = serde_json::from_slice(&nightly).unwrap();
    assert_eq!(value["channel"], "nightly");
    assert!(serde_json::from_slice::<FieldManifest>(&nightly).is_err());

    assert!(
        !release(&["manifest", "0.1.0", "beta", dir.to_str().unwrap()])
            .status
            .success()
    );
    fs::remove_dir_all(&dir).unwrap();
}

fn v(text: &str) -> Version {
    text.parse().unwrap()
}

fn nightly(cargo: &str, date: &str, time: &str) -> Version {
    v(&stdout(release(&[
        "nightly-version",
        cargo,
        date,
        time,
        "61697fa",
    ])))
}

/// A nightly is newer than the version in Cargo.toml and older than the next stable; a later
/// build is newer. Units compare without the build metadata (the commit).
#[test]
fn a_nightly_version_sorts_between_releases() {
    let night = nightly("0.1.0-alpha.10", "20261112", "0317");
    assert_eq!(night.to_string(), "0.1.0-nightly.20261112.317+61697fa");
    assert_eq!(night.pre.split('.').next(), Some("nightly"));
    assert!(v("0.1.0-alpha.10") < night && night < v("0.1.0"));

    let after_stable = nightly("0.1.0", "20261112", "0905");
    assert_eq!(
        after_stable.to_string(),
        "0.1.1-nightly.20261112.905+61697fa"
    );
    assert!(v("0.1.0") < after_stable && after_stable < v("0.1.1"));

    let later = nightly("0.1.0", "20261112", "1200");
    let next_day = nightly("0.1.0", "20261113", "0000");
    assert!(after_stable.cmp_precedence(&later).is_lt());
    assert!(later.cmp_precedence(&next_day).is_lt());

    for bad in [
        ["1.2", "20261112", "0000", "abc"],
        ["0.1.0", "2026111", "0000", "abc"],
        ["0.1.0", "20261112", "1260", "abc"],
        ["0.1.0", "20261112", "0000", "ABC"],
    ] {
        let mut args = vec!["nightly-version"];
        args.extend(bad);
        assert!(!release(&args).status.success(), "{bad:?}");
    }
}

/// The stable version is the one in Cargo.toml, which a `v*` tag must name.
#[test]
fn the_stable_version_is_cargo_toml() {
    assert_eq!(stdout(release(&["version"])), env!("CARGO_PKG_VERSION"));
}
