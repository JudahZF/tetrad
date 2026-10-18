//! Read-only audit of an explicit external factory preset directory.
//! Prints aggregate counts only. All edits happen in memory.
use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use tetrad_preset::{Limits, ParamValue, Preset, PresetDocument, Severity};

#[derive(Default)]
struct Counts {
    files: usize,
    bytes: usize,
    roundtrips: usize,
    typed_roundtrips: usize,
    chains: usize,
    model_slots: usize,
    parameters: usize,
    scene_values: usize,
    unknown_fields: usize,
    errors: usize,
    warnings: usize,
    info: usize,
    rename_edits: usize,
    scene_label_edits: usize,
    parameter_edits: usize,
    unchanged_sources: usize,
    diagnostic_codes: BTreeMap<String, usize>,
}

fn bounded_read(path: &Path, max: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(io::Error::other("corpus file exceeds byte limit"));
    }
    Ok(bytes)
}

fn audit(paths: &[PathBuf]) -> Result<Counts, Box<dyn Error>> {
    let limits = Limits::default();
    let mut counts = Counts::default();
    for path in paths {
        let bytes = bounded_read(path, limits.max_bytes)?;
        let doc = PresetDocument::parse(&bytes, limits)?;
        if doc.to_bytes() != bytes {
            return Err("no-op corpus roundtrip differs".into());
        }
        counts.roundtrips += 1;
        if Preset::try_from(&doc)?.to_bytes() != bytes {
            return Err("typed stock-layout save differs from the stock file".into());
        }
        counts.typed_roundtrips += 1;
        let summary = doc.summary()?;
        counts.files += 1;
        counts.bytes += bytes.len();
        counts.chains += summary.chain_count;
        counts.model_slots += summary.model_slot_count;
        counts.parameters += summary.parameter_count;
        counts.scene_values += summary.scene_value_count;
        counts.unknown_fields += summary.unknown_field_count;
        for diagnostic in doc.validate()? {
            *counts
                .diagnostic_codes
                .entry(format!(
                    "{}:{}",
                    diagnostic.code,
                    diagnostic
                        .path
                        .rsplit('.')
                        .next()
                        .unwrap_or("root")
                        .split('[')
                        .next()
                        .unwrap_or("root")
                ))
                .or_default() += 1;
            match diagnostic.severity {
                Severity::Error => counts.errors += 1,
                Severity::Warning => counts.warnings += 1,
                Severity::Info => counts.info += 1,
            }
        }
        let mut renamed = doc.clone();
        renamed.rename("synthetic audit name")?;
        if renamed.summary()?.name.as_deref() != Some("synthetic audit name")
            || renamed.summary()?.hash != summary.hash
        {
            return Err("rename contract failed".into());
        }
        counts.rename_edits += 1;
        if let Some(scene) = summary.scenes.iter().find(|s| s.label.is_some()) {
            let mut labeled = doc.clone();
            labeled.set_scene_label(scene.index, "synthetic audit label")?;
            if labeled.summary()?.scenes[scene.index].label.as_deref()
                != Some("synthetic audit label")
            {
                return Err("scene label contract failed".into());
            }
            counts.scene_label_edits += 1;
        }
        let mut selection = None;
        'chain: for chain in &summary.chains {
            for model in &chain.models {
                for param in &model.params {
                    if !param.values.is_empty() {
                        selection = Some((chain.position, model.position, param.position));
                        break 'chain;
                    }
                }
            }
        }
        if let Some((chain, model, param)) = selection {
            let mut edited = doc.clone();
            edited.set_parameter(
                chain,
                model,
                param,
                0,
                ParamValue::String("synthetic audit value".into()),
            )?;
            if edited.summary()?.chains[chain].models[model].params[param].values[0]
                != Some(ParamValue::String("synthetic audit value".into()))
            {
                return Err("parameter edit contract failed".into());
            }
            counts.parameter_edits += 1;
        }
        if bounded_read(path, limits.max_bytes)? != bytes {
            return Err("external corpus source changed during audit".into());
        }
        counts.unchanged_sources += 1;
    }
    Ok(counts)
}

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("pass an explicit external factory preset directory")?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.path().extension().is_some_and(|e| e == "pb") {
            paths.push(entry.path());
        }
        if paths.len() > 10_000 {
            return Err("corpus file count exceeds 10000".into());
        }
    }
    paths.sort();
    if paths.is_empty() {
        return Err("external corpus contains no .pb presets".into());
    }
    let c = audit(&paths)?;
    println!(
        "files={} bytes={} roundtrips={} typed_roundtrips={} chains={} model_slots={} parameters={} scene_values={} unknown_fields={} diagnostic_errors={} diagnostic_warnings={} diagnostic_info={} rename_edits={} scene_label_edits={} parameter_edits={} unchanged_sources={}",
        c.files,
        c.bytes,
        c.roundtrips,
        c.typed_roundtrips,
        c.chains,
        c.model_slots,
        c.parameters,
        c.scene_values,
        c.unknown_fields,
        c.errors,
        c.warnings,
        c.info,
        c.rename_edits,
        c.scene_label_edits,
        c.parameter_edits,
        c.unchanged_sources
    );
    for (code, count) in c.diagnostic_codes {
        println!("diagnostic={code} count={count}");
    }
    if c.errors > 0 {
        return Err("corpus has schema validation errors; wire roundtrips and edits are reported separately".into());
    }
    Ok(())
}
