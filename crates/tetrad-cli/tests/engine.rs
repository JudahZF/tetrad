use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// `(category, model)` pairs: roles follow the category, so each model sits in its stock one.
const REQUIRED_MODELS: [(u32, u32); 5] = [
    (10, 10004),
    (11, 11000),
    (23, 23000),
    (25, 25000),
    (28, 28000),
];
const EMPTY_REPORT: &str = "<ModuleReport/>";

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-engine-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let categories: String = REQUIRED_MODELS
            .iter()
            .map(|&(category, id)| {
                format!(
                    "<Category id='{category}' name='C{category}'>{}</Category>",
                    model_xml(id)
                )
            })
            .collect();
        fs::write(
            root.join("ModelRepo.xml"),
            format!("<Models>{categories}</Models>"),
        )
        .unwrap();
        fs::write(root.join("moduleReport.xml"), EMPTY_REPORT).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
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
         </Model>"
    )
}

#[test]
fn compile_places_a_default_preset_into_per_core_run_lists() {
    let workspace = Workspace::new();
    let preset = workspace.path("default.pb");
    let catalog = workspace.root.to_str().unwrap();
    let model_repo = workspace.path("ModelRepo.xml");
    let module_report = workspace.path("moduleReport.xml");

    let created = workspace.cli(&[
        "preset",
        "new",
        "--output",
        preset.to_str().unwrap(),
        "--model-repo",
        model_repo.to_str().unwrap(),
        "--module-report",
        module_report.to_str().unwrap(),
    ]);
    assert!(created.status.success(), "{created:?}");

    let output = workspace.cli(&[
        "engine",
        "compile",
        "--input",
        preset.to_str().unwrap(),
        "--catalog",
        catalog,
    ]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["execution"], "none; compile only");
    assert_eq!(report["defaults"]["absent_scalars"], "Proto3Default");
    let cores = report["cores"].as_array().unwrap();
    assert_eq!(cores.len(), 4);
    // The run list order is tetrad-engine's (stock router set); the CLI only reports it.
    let io_core = cores[1]["run_list"].as_array().unwrap();
    assert!(!io_core.is_empty());
    for (slot, entry) in io_core.iter().enumerate() {
        assert_eq!(entry["placement"], json!({ "core": 1, "slot": slot }));
    }
}
