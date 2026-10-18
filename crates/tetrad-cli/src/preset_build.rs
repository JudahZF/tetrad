//! Preset migration, new presets and grid-block edits over decrypted catalog
//! files. Nothing here reads a device or compiles a DSP graph.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tetrad_catalog::{Catalog, Specs};
use tetrad_preset::{
    CatalogModel, CatalogParam, Limits, MigrationReport, Model, ModelCatalog, ParamValue, Preset,
    PresetDocument, Severity, Slot, SplitterType,
};

use crate::{CatalogFiles, Result, io, print_json, read_xml};

/// Categories whose stored parameter list is longer than the XML list (cabinet data).
const DYNAMIC_PARAMETER_CATEGORIES: [u32; 2] = [12, 21];

#[derive(Args)]
pub struct Migrate {
    #[arg(long)]
    input: PathBuf,
    /// Must not exist. The input is never overwritten.
    #[arg(long)]
    output: PathBuf,
    /// Decrypted ModelRepo XML. Without both catalog files, migration uses no
    /// model metadata and keeps every grid block it cannot describe.
    #[arg(long, requires = "module_report")]
    model_repo: Option<PathBuf>,
    #[arg(long, requires = "model_repo")]
    module_report: Option<PathBuf>,
}

#[derive(Args)]
pub struct New {
    /// Must not exist.
    #[arg(long)]
    output: PathBuf,
    #[command(flatten)]
    catalog: CatalogFiles,
}

#[derive(Subcommand)]
pub enum BlockOperation {
    /// Place a model with catalog default values in an empty slot.
    Insert {
        #[command(flatten)]
        files: BlockFiles,
        #[command(flatten)]
        at: Position,
        #[arg(long)]
        model_id: u32,
    },
    /// Empty a slot and report the removed model.
    Remove {
        #[command(flatten)]
        files: BlockFiles,
        #[command(flatten)]
        at: Position,
    },
    /// Move a block into an empty slot.
    Move {
        #[command(flatten)]
        files: BlockFiles,
        #[command(flatten)]
        from: Position,
        #[command(flatten)]
        to: Destination,
    },
    /// Replace a slot's model with catalog default values.
    Replace {
        #[command(flatten)]
        files: BlockFiles,
        #[command(flatten)]
        at: Position,
        #[arg(long)]
        model_id: u32,
    },
}

#[derive(Args)]
pub struct BlockFiles {
    /// Input preset; never overwritten.
    #[arg(long)]
    input: PathBuf,
    /// Must not exist.
    #[arg(long)]
    output: PathBuf,
    #[command(flatten)]
    catalog: CatalogFiles,
}

/// Grid position, starting at row 0 and column 0.
#[derive(Args)]
pub struct Position {
    #[arg(long)]
    row: usize,
    #[arg(long)]
    column: usize,
}

impl Position {
    fn slot(&self) -> Slot {
        Slot {
            row: self.row,
            column: self.column,
        }
    }
}

#[derive(Args)]
pub struct Destination {
    #[arg(long)]
    to_row: usize,
    #[arg(long)]
    to_column: usize,
}

/// tetrad-catalog as the tetrad-preset model catalog. Mirrors the adapter in
/// `tetrad-app`; splitter and tempo conversions are not wired.
struct SpecsCatalog<'a>(&'a Specs);

impl ModelCatalog for SpecsCatalog<'_> {
    fn model(&self, model_id: u32) -> Option<CatalogModel> {
        let spec = self.0.models.get(&model_id)?;
        if DYNAMIC_PARAMETER_CATEGORIES.contains(&spec.category_id) {
            return None;
        }
        let params = spec.parameters.iter().map(|param| CatalogParam {
            default: match &param.default {
                Some(tetrad_catalog::ParamDefault::Text(text)) => ParamValue::String(text.clone()),
                _ => ParamValue::Float(param.default_normalized()),
            },
            expression_assignable: param.exp_assignable.unwrap_or(true),
        });
        // MUTE follows the XML parameters; BYPASS is not stored.
        let mute = CatalogParam {
            default: ParamValue::Float(0.0),
            expression_assignable: false,
        };
        Some(CatalogModel {
            params: params.chain([mute]).collect(),
            sidechain_parameter: spec
                .sidechain
                .as_ref()
                .map(|sidechain| sidechain.source_param as usize),
        })
    }

    fn splitter_type(&self, _: SplitterType) -> Option<f32> {
        None
    }

    fn tempo_value(&self, _: u32) -> Option<f32> {
        None
    }
}

struct EmptyCatalog;

impl ModelCatalog for EmptyCatalog {
    fn model(&self, _: u32) -> Option<CatalogModel> {
        None
    }

    fn splitter_type(&self, _: SplitterType) -> Option<f32> {
        None
    }

    fn tempo_value(&self, _: u32) -> Option<f32> {
        None
    }
}

pub(crate) fn load_specs(model_repo: &Path, module_report: &Path) -> Result<Specs> {
    let catalog = Catalog::parse(&read_xml(model_repo)?, &read_xml(module_report)?)?;
    Ok(Specs::resolve(&catalog)?)
}

pub fn migrate(args: Migrate) -> Result<bool> {
    let bytes = io::read_bounded(&args.input, Limits::default().max_bytes)?;
    let mut document = PresetDocument::parse(&bytes, Limits::default())?;
    let report = match (&args.model_repo, &args.module_report) {
        (Some(model_repo), Some(module_report)) => {
            let specs = load_specs(model_repo, module_report)?;
            document.modify(|preset| preset.migrate(&SpecsCatalog(&specs)))?
        }
        _ => document.modify(|preset| preset.migrate(&EmptyCatalog))?,
    };
    let encoded = document.to_bytes();
    io::write_new(&args.output, &encoded)?;
    print_json(&json!({
        "input": args.input.to_string_lossy(),
        "output": args.output.to_string_lossy(),
        "input_bytes": bytes.len(),
        "output_bytes": encoded.len(),
        "catalog_supplied": args.model_repo.is_some(),
        "catalog_complete": false,
        "migration": migration_json(&report),
        "hardware_compatibility": "unverified"
    }))?;
    Ok(true)
}

fn migration_json(report: &MigrationReport) -> Value {
    let unknown_blocks: Vec<Value> = report
        .unknown_blocks
        .iter()
        .map(|block| {
            json!({
                "row": block.slot.row,
                "column": block.slot.column,
                "model_id": block.model_id
            })
        })
        .collect();
    json!({
        "imported_models": report.imported_models,
        "resized_models": report.resized_models,
        "unknown_blocks": unknown_blocks,
        "unknown_blocks_emptied": report.unknown_blocks_emptied,
        "default_controls": report.default_controls,
        "converted_splitters": report.converted_splitters,
        "converted_tempo": report.converted_tempo,
        "converted_stomps": report.converted_stomps,
        "converted_midi": report.converted_midi,
        "sidechain_parameters": report.sidechain_parameters,
        "relayout": report.relayout
    })
}

pub fn new(args: New) -> Result<bool> {
    let specs = load_specs(&args.catalog.model_repo, &args.catalog.module_report)?;
    let encoded =
        Preset::new_default(tetrad_firmware::running().version, &SpecsCatalog(&specs))?.to_bytes();
    let diagnostics = PresetDocument::parse(&encoded, Limits::default())?.validate()?;
    let errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .count();
    io::write_new(&args.output, &encoded)?;
    print_json(&json!({
        "output": args.output.to_string_lossy(),
        "output_bytes": encoded.len(),
        "firmware_version": tetrad_firmware::running().version,
        "validation_errors": errors,
        "diagnostics": diagnostics,
        "hardware_compatibility": "unverified",
        "note": "Unsaved default preset; no save stamp, DSP compilation or hardware import was performed"
    }))?;
    Ok(errors == 0)
}

pub fn edit_block(operation: BlockOperation) -> Result<bool> {
    match operation {
        BlockOperation::Insert {
            files,
            at,
            model_id,
        } => apply(&files, |preset, catalog| {
            let model = block_model(model_id, catalog)?;
            preset.insert_block(at.slot(), model, catalog)?;
            Ok(
                json!({ "operation": "insert", "row": at.row, "column": at.column, "model_id": model_id }),
            )
        }),
        BlockOperation::Remove { files, at } => apply(&files, |preset, catalog| {
            let removed = preset.remove_block(at.slot(), catalog)?;
            Ok(json!({
                "operation": "remove", "row": at.row, "column": at.column,
                "removed_model_id": removed.hash
            }))
        }),
        BlockOperation::Move { files, from, to } => apply(&files, |preset, catalog| {
            let destination = Slot {
                row: to.to_row,
                column: to.to_column,
            };
            preset.move_block(from.slot(), destination, catalog)?;
            Ok(json!({
                "operation": "move", "from_row": from.row, "from_column": from.column,
                "to_row": to.to_row, "to_column": to.to_column
            }))
        }),
        BlockOperation::Replace {
            files,
            at,
            model_id,
        } => apply(&files, |preset, catalog| {
            let model = block_model(model_id, catalog)?;
            let replaced = preset.replace_model(at.slot(), model)?;
            Ok(json!({
                "operation": "replace", "row": at.row, "column": at.column,
                "replaced_model_id": replaced.hash, "model_id": model_id
            }))
        }),
    }
}

fn block_model(model_id: u32, catalog: &impl ModelCatalog) -> tetrad_preset::Result<Model> {
    let metadata = catalog
        .model(model_id)
        .ok_or(tetrad_preset::Error::UnknownModel(model_id))?;
    Ok(Model::block(
        model_id,
        metadata.params.into_iter().map(|param| param.default),
    ))
}

fn apply(
    files: &BlockFiles,
    edit: impl FnOnce(&mut Preset, &SpecsCatalog<'_>) -> tetrad_preset::Result<Value>,
) -> Result<bool> {
    let specs = load_specs(&files.catalog.model_repo, &files.catalog.module_report)?;
    let catalog = SpecsCatalog(&specs);
    let bytes = io::read_bounded(&files.input, Limits::default().max_bytes)?;
    let mut document = PresetDocument::parse(&bytes, Limits::default())?;
    let detail = document.modify(|preset| edit(preset, &catalog))?;
    let encoded = document.to_bytes();
    io::write_new(&files.output, &encoded)?;
    print_json(&json!({
        "output": files.output.to_string_lossy(),
        "input_bytes": bytes.len(),
        "output_bytes": encoded.len(),
        "changed": encoded != bytes,
        "edit": detail,
        "hardware_compatibility": "unverified",
        "note": "Grid edit only; no DSP compilation or hardware import was performed"
    }))?;
    Ok(true)
}
