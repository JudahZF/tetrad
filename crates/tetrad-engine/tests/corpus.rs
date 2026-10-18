//! Opt-in corpus checks against local stock data in `QC_CORPUS_DIR` (README, "Corpus tests").
//! Nothing here is committed data: `plain/` holds plaintext `ModelRepo.xml`/`moduleReport.xml`,
//! `presets/` a directory tree of stock `.pb` presets. Each test skips with a message when its
//! data is absent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tetrad_catalog::{Catalog, Specs};
use tetrad_engine::{
    AbsentScalars, CompileOptions, Globals, Layout, StoredMasks, compile_stored, fit, fit_stored,
    import_layout,
};
use tetrad_preset::{Limits, PresetDocument};

const OPTIONS: CompileOptions = CompileOptions {
    globals: Globals {
        input_gate: true,
        global_eq: true,
    },
    ddc: true,
};

/// The catalog and every stock preset file, or `None` (with a note) when `QC_CORPUS_DIR`
/// does not hold `plain/` and `presets/`.
fn corpus() -> Option<(Specs, Vec<PathBuf>)> {
    let root = std::env::var_os("QC_CORPUS_DIR").map(PathBuf::from);
    let dir = |name| {
        root.as_ref()
            .map(|root| root.join(name))
            .filter(|p| p.is_dir())
    };
    let (Some(catalog), Some(presets_dir)) = (dir("plain"), dir("presets")) else {
        eprintln!("skipped: set QC_CORPUS_DIR (with plain/ and presets/)");
        return None;
    };
    let models = std::fs::read_to_string(catalog.join("ModelRepo.xml")).unwrap();
    let report = std::fs::read_to_string(catalog.join("moduleReport.xml")).unwrap();
    let specs = Specs::resolve(&Catalog::parse(&models, &report).unwrap()).unwrap();
    let mut files = Vec::new();
    presets(&presets_dir, &mut files);
    files.sort();
    Some((specs, files))
}

/// The preset's stored core masks (`layout_code_1/2`) and EQ words.
fn stored(file: &Path) -> StoredMasks {
    let bytes = std::fs::read(file).unwrap();
    tetrad_preset::Preset::decode(&bytes, Limits::default())
        .map_or(StoredMasks::default(), |p| StoredMasks::from_preset(&p))
}

fn layout(file: &Path, specs: &Specs) -> Result<Layout, String> {
    let bytes = std::fs::read(file).unwrap();
    let summary = PresetDocument::parse(&bytes, Limits::default())
        .and_then(|document| document.summary())
        .map_err(|error| format!("parse: {error}"))?;
    import_layout(&summary, specs, AbsentScalars::Proto3Default)
        .map(|imported| imported.layout)
        .map_err(|error| format!("import: {error}"))
}

fn presets(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            presets(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "pb")
            && !path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                // Metadata files share the extension but are not presets.
                name.starts_with("__") || name.starts_with("factory_")
            })
        {
            out.push(path);
        }
    }
}

#[test]
#[ignore = "requires QC_CORPUS_DIR"]
fn stock_presets_import_route_and_fit() {
    let Some((specs, files)) = corpus() else {
        return;
    };
    let mut failures = BTreeMap::<String, Vec<String>>::new();
    let (mut inhibited, mut max_slots, mut links, mut stored_masks) = (0, 0, 0, 0);
    for file in &files {
        let outcome = layout(file, &specs).and_then(|layout| {
            // The stock load path: the stored masks first (`0x681d44`).
            let stored = stored(file);
            stored_masks += stored.masks.iter().flatten().count();
            let plan = compile_stored(&layout, &specs, OPTIONS, stored)
                .map_err(|error| format!("compile: {error}"))?;
            inhibited += usize::from(plan.inhibited.global_eq || plan.inhibited.input_gate);
            links += plan.links.len();
            let slots = plan.cores.iter().map(|core| core.slots.len()).max();
            max_slots = max_slots.max(slots.unwrap_or(0));
            // The router builder (`0x87fc2c`) never uses more slots than the
            // fit's cost model (`0x5fb108`) charged.
            let fit = fit_stored(&layout, &specs, OPTIONS.globals, stored).unwrap();
            let built = plan.cores.each_ref().map(|core| core.slots.len() as u32);
            if built.iter().zip(&fit.slots).any(|(built, fit)| built > fit) {
                return Err(format!("slots: built {built:?}, fit {:?}", fit.slots));
            }
            Ok(())
        });
        if let Err(error) = outcome {
            let kind: String = error
                .chars()
                .filter(|c| !c.is_ascii_digit())
                .take(80)
                .collect();
            failures
                .entry(kind)
                .or_default()
                .push(format!("{}: {error}", file.display()));
        }
    }
    for (kind, files) in &failures {
        eprintln!("{} × {kind}\n    e.g. {}", files.len(), files[0]);
    }
    let failed: usize = failures.values().map(Vec::len).sum();
    eprintln!("{} presets, {failed} failed", files.len());
    eprintln!("{inhibited} drop a global; {links} links; at most {max_slots} slots on a core");
    eprintln!("{stored_masks} stored group masks tried");
    assert_eq!(failed, 0);
}

/// Fit regression probe. Writes one line per preset with the core of every
/// item per group to `TETRAD_FIT_PROBE_OUT`; with `TETRAD_FIT_PROBE_BASELINE` (an
/// earlier output) it reports how many groups changed their core count or
/// their assignment. Report only: stock core counts are not in the corpus.
#[test]
#[ignore = "requires the corpus and TETRAD_FIT_PROBE_OUT"]
fn fit_core_assignment_probe() {
    let Some(out) = std::env::var_os("TETRAD_FIT_PROBE_OUT") else {
        eprintln!("skipped: set TETRAD_FIT_PROBE_OUT");
        return;
    };
    let Some((specs, files)) = corpus() else {
        return;
    };
    let mut lines = Vec::new();
    for file in &files {
        let groups = match layout(file, &specs) {
            Ok(layout) => match fit(&layout, &specs, OPTIONS.globals, None) {
                Ok(fit) => fit
                    .cores
                    .iter()
                    .map(|cores| cores.iter().map(|core| core.index().to_string()).collect())
                    .collect::<Vec<String>>()
                    .join("|"),
                Err(error) => format!("fit: {error}"),
            },
            Err(error) => error,
        };
        lines.push(format!("{}\t{groups}", file.display()));
    }
    std::fs::write(&out, lines.join("\n")).unwrap();
    let Some(baseline) = std::env::var_os("TETRAD_FIT_PROBE_BASELINE") else {
        return;
    };
    let before: BTreeMap<String, String> = std::fs::read_to_string(baseline)
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(file, groups)| (file.to_owned(), groups.to_owned()))
        .collect();
    let count = |cores: &str| {
        let mut used: Vec<char> = cores.chars().collect();
        used.sort_unstable();
        used.dedup();
        used.len()
    };
    let (mut groups, mut count_changed, mut moved) = (0, 0, 0);
    for line in &lines {
        let (file, now) = line.split_once('\t').unwrap();
        let Some(old) = before.get(file) else {
            continue;
        };
        for (now, old) in now.split('|').zip(old.split('|')) {
            groups += 1;
            count_changed += usize::from(count(now) != count(old));
            moved += usize::from(now != old);
        }
    }
    eprintln!(
        "{groups} groups compared: {count_changed} changed core count, {moved} changed assignment"
    );
}
