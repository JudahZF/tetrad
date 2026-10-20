//! The publishing path end to end: keygen, package, catalog, then what the device verifies.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tetrad_extensions::catalog::{Catalog, PublicKey};
use tetrad_extensions::package::Package;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tetrad_ext(args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_tetrad-ext"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tetrad-ext {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn extension(dir: &Path, version: &str) {
    fs::create_dir_all(dir.join("dist")).unwrap();
    fs::write(
        dir.join("tetrad-extension.json"),
        format!(r#"{{"id": "test.ext", "name": "Test", "version": "{version}", "api": 1}}"#),
    )
    .unwrap();
    fs::write(
        dir.join("dist/index.mjs"),
        "export default { activate() {} };",
    )
    .unwrap();
}

#[test]
fn package_catalog_verify_round_trip() {
    let tmp =
        TempDir(std::env::temp_dir().join(format!("tetrad-ext-publish-{}", std::process::id())));
    let _ = fs::remove_dir_all(&tmp.0);
    let path = |p: &str| tmp.0.join(p).to_str().unwrap().to_owned();

    tetrad_ext(&["keygen", "--out", &path("keys")]);
    let mode = fs::metadata(tmp.0.join("keys/signing.key")).unwrap();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777,
        0o600
    );

    let publish = |version: &str, tag: &str| {
        extension(&tmp.0.join("ext"), version);
        tetrad_ext(&[
            "package",
            &path("ext"),
            "--out",
            &path(&format!("repo/packages/test.ext-{version}.tetradext")),
        ]);
        tetrad_ext(&[
            "catalog",
            "--repo-dir",
            &path("repo"),
            "--key",
            &path("keys/signing.key"),
            "--tag",
            tag,
        ]);
    };
    publish("1.0.0", "r1");
    publish("1.1.0", "r2");

    let read = |p: &str| fs::read(tmp.0.join(p)).unwrap();
    let key =
        PublicKey::from_base64(&String::from_utf8(read("keys/tetrad-extensions.pub")).unwrap())
            .unwrap();
    let catalog = Catalog::verify(
        &read("repo/tetrad-extensions.json"),
        &read("repo/tetrad-extensions.sig"),
        &key,
    )
    .unwrap();
    let tags: Vec<(String, &str)> = catalog
        .extensions
        .iter()
        .map(|e| (e.version.to_string(), e.asset.tag.as_str()))
        .collect();
    assert_eq!(
        tags,
        [("1.0.0".into(), "r1"), ("1.1.0".into(), "r2")],
        "a published package keeps its release tag"
    );

    let latest = catalog.latest("test.ext").unwrap();
    let bytes = read(&format!("repo/packages/{}", latest.asset.name));
    assert_eq!(bytes.len() as u64, latest.asset.size);
    assert_eq!(
        Package::decode(&bytes).unwrap().manifest.version,
        latest.version
    );
    assert_eq!(
        read("repo/tetrad-extensions.pub"),
        read("keys/tetrad-extensions.pub")
    );

    // A development store of that repository loads on the device path with the newest
    // version installed and the catalog still verified against the pinned key.
    tetrad_ext(&[
        "stage",
        "--repo-dir",
        &path("repo"),
        "--source",
        "tester/exts",
        "--store",
        &path("store"),
    ]);
    let state = tetrad_extensions::store::Store::open(tmp.0.join("store"))
        .unwrap()
        .load();
    assert!(state.problems.is_empty(), "{:?}", state.problems);
    let installed: Vec<String> = (state.installed.iter())
        .map(|i| format!("{} {}", i.key, i.current.version))
        .collect();
    assert_eq!(installed, ["tester/exts:test.ext 1.1.0"]);
    assert_eq!(state.catalogs.len(), 1);
}
