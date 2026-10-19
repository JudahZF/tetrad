//! Corpus check against `$QC_CORPUS_DIR/rootfs/media/p4` (docs/development.md, "Corpus tests").
//! Ignored by default. Never prints values: AccountConfig.ini holds tokens.

use std::path::PathBuf;

use crate::{DeviceSettings, Hardware, SettingsFile, SettingsFileId, SettingsStorage};

/// `$QC_CORPUS_DIR/rootfs/media/p4`, or `None` with a note when the variable is not set.
fn p4() -> Option<PathBuf> {
    let Some(dir) = std::env::var_os("QC_CORPUS_DIR") else {
        eprintln!("skipped: set QC_CORPUS_DIR");
        return None;
    };
    Some(PathBuf::from(dir).join("rootfs/media/p4"))
}

#[test]
#[ignore = "reads $QC_CORPUS_DIR/rootfs/media/p4"]
fn stock_snapshot_files_round_trip_byte_exact() {
    let Some(p4) = p4() else { return };
    for name in [
        "ZenjackConfig.ini",
        "PermanentSettings.ini",
        "AccountConfig.ini",
        "PluginLicenses.ini",
    ] {
        let path = p4.join(name);
        let Ok(data) = std::fs::read(&path) else {
            eprintln!("skip {name}: not present");
            continue;
        };
        let (file, issues) = SettingsFile::parse(&data);
        assert!(issues.is_empty(), "{name}: {} format issues", issues.len());
        let out = file.to_bytes();
        let first_diff = out.iter().zip(&data).position(|(a, b)| a != b);
        assert!(
            out == data,
            "{name}: differs at byte {first_diff:?} (lengths {} vs {})",
            out.len(),
            data.len()
        );
        for key in file.keys() {
            assert!(
                file.value(key).is_some_and(|v| v.is_ok()),
                "{name}: {key} does not decode"
            );
        }
    }
}

/// The settings files of a p4 copy.
struct SnapshotStorage(PathBuf);

impl SettingsStorage for SnapshotStorage {
    fn read(&self, file: SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
        let name = file.path().rsplit('/').next().unwrap_or_default();
        Ok(std::fs::read(self.0.join(name)).ok())
    }
    fn write(&mut self, _: SettingsFileId, _: &[u8]) -> std::io::Result<()> {
        unreachable!("corpus test never writes")
    }
    fn remove(&mut self, _: SettingsFileId) -> std::io::Result<()> {
        unreachable!("corpus test never removes")
    }
}

/// The typed model loads the snapshot and does not dirty any file.
#[test]
#[ignore = "reads $QC_CORPUS_DIR/rootfs/media/p4"]
fn stock_snapshot_loads_typed_without_changes() {
    let Some(p4) = p4() else { return };
    let (mut device, issues) =
        DeviceSettings::load(&SnapshotStorage(p4), Hardware::QuadCortex).unwrap();
    // Key names only; issue values may hold user data.
    let keys: Vec<Option<&str>> = issues.iter().map(|i| i.key.as_deref()).collect();
    assert!(keys.is_empty(), "issues at {keys:?}");
    assert!(device.update(|_| {}).is_empty());
    assert_eq!(device.dirty_files().count(), 0);
}
