use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

const REQUIRED_MODELS: [u32; 5] = [10004, 11000, 23000, 25000, 28000];
const BLOCK_MODEL: u32 = 1000;
const EMPTY_REPORT: &str = "<ModuleReport/>";

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-preset-build-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn write_catalog(&self, prefix: &str, model_ids: &[u32]) -> (PathBuf, PathBuf) {
        let models: String = model_ids.iter().map(|&id| model_xml(id)).collect();
        let repo = self.path(&format!("{prefix}-repo.xml"));
        let report = self.path(&format!("{prefix}-report.xml"));
        fs::write(
            &repo,
            format!("<Models><Category id='7' name='Utility'>{models}</Category></Models>"),
        )
        .unwrap();
        fs::write(&report, EMPTY_REPORT).unwrap();
        (repo, report)
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .arg("preset")
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn model_xml(id: u32) -> String {
    format!(
        "<Model id='{id}' name='M{id}'>\
         <Parameter name='A' type='float' index='0' defaultValue='0.25' min='0' max='1'/>\
         <Parameter name='B' type='float' index='1' defaultValue='0.75' min='0' max='1'/>\
         </Model>"
    )
}

fn json_report(output: Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn migrate_keeps_blocks_the_catalog_cannot_describe() {
    let workspace = Workspace::new();
    let mut full: Vec<u32> = REQUIRED_MODELS.to_vec();
    full.push(BLOCK_MODEL);
    let (full_repo, report) = workspace.write_catalog("full", &full);
    let (partial_repo, _) = workspace.write_catalog("partial", &REQUIRED_MODELS);
    let new_preset = workspace.path("new.pb");
    let with_block = workspace.path("with-block.pb");
    let migrated = workspace.path("migrated.pb");

    json_report(workspace.cli(&[
        "new",
        "--output",
        path_str(&new_preset),
        "--model-repo",
        path_str(&full_repo),
        "--module-report",
        path_str(&report),
    ]));
    json_report(workspace.cli(&[
        "edit-block",
        "insert",
        "--input",
        path_str(&new_preset),
        "--output",
        path_str(&with_block),
        "--model-repo",
        path_str(&full_repo),
        "--module-report",
        path_str(&report),
        "--row",
        "0",
        "--column",
        "0",
        "--model-id",
        &BLOCK_MODEL.to_string(),
    ]));

    let result = json_report(workspace.cli(&[
        "migrate",
        "--input",
        path_str(&with_block),
        "--output",
        path_str(&migrated),
        "--model-repo",
        path_str(&partial_repo),
        "--module-report",
        path_str(&report),
    ]));
    assert_eq!(result["catalog_complete"], false);
    assert_eq!(result["migration"]["unknown_blocks_emptied"], false);
    assert_eq!(
        result["migration"]["unknown_blocks"],
        json!([{ "row": 0, "column": 0, "model_id": BLOCK_MODEL }])
    );
    assert!(migrated.exists());
}

#[test]
fn new_writes_a_default_preset_from_a_synthetic_catalog() {
    let workspace = Workspace::new();
    let (repo, report) = workspace.write_catalog("catalog", &REQUIRED_MODELS);
    let output = workspace.path("default.pb");

    let result = json_report(workspace.cli(&[
        "new",
        "--output",
        path_str(&output),
        "--model-repo",
        path_str(&repo),
        "--module-report",
        path_str(&report),
    ]));
    assert_eq!(result["firmware_version"], "4.1.0");
    assert_eq!(result["validation_errors"], 0);
    assert_eq!(result["hardware_compatibility"], "unverified");
    assert!(output.exists());

    let refused = workspace.cli(&[
        "new",
        "--output",
        path_str(&output),
        "--model-repo",
        path_str(&repo),
        "--module-report",
        path_str(&report),
    ]);
    assert_eq!(refused.status.code(), Some(1));
}

#[test]
fn edit_block_inserts_moves_and_removes_a_grid_block() {
    let workspace = Workspace::new();
    let mut full: Vec<u32> = REQUIRED_MODELS.to_vec();
    full.push(BLOCK_MODEL);
    let (repo, report) = workspace.write_catalog("full", &full);
    let new_preset = workspace.path("new.pb");
    let inserted = workspace.path("inserted.pb");
    let moved = workspace.path("moved.pb");
    let removed = workspace.path("removed.pb");
    let catalog = [
        "--model-repo",
        path_str(&repo),
        "--module-report",
        path_str(&report),
    ];

    json_report(workspace.cli(&[
        "new",
        "--output",
        path_str(&new_preset),
        "--model-repo",
        path_str(&repo),
        "--module-report",
        path_str(&report),
    ]));

    let mut insert = vec![
        "edit-block",
        "insert",
        "--input",
        path_str(&new_preset),
        "--output",
        path_str(&inserted),
        "--row",
        "0",
        "--column",
        "0",
        "--model-id",
    ];
    let model = BLOCK_MODEL.to_string();
    insert.push(&model);
    insert.extend(catalog);
    let result = json_report(workspace.cli(&insert));
    assert_eq!(result["changed"], true);
    assert_eq!(result["edit"]["operation"], "insert");

    let result = json_report(workspace.cli(&[
        "edit-block",
        "move",
        "--input",
        path_str(&inserted),
        "--output",
        path_str(&moved),
        "--row",
        "0",
        "--column",
        "0",
        "--to-row",
        "1",
        "--to-column",
        "0",
        catalog[0],
        catalog[1],
        catalog[2],
        catalog[3],
    ]));
    assert_eq!(result["edit"]["operation"], "move");

    let result = json_report(workspace.cli(&[
        "edit-block",
        "remove",
        "--input",
        path_str(&moved),
        "--output",
        path_str(&removed),
        "--row",
        "1",
        "--column",
        "0",
        catalog[0],
        catalog[1],
        catalog[2],
        catalog[3],
    ]));
    assert_eq!(result["edit"]["removed_model_id"], BLOCK_MODEL);
}
