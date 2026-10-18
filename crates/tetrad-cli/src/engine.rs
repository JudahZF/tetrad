//! Imports a saved preset, then routes, fits and compiles it into per-core run lists.
//! Compile only: nothing is sent to a device.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tetrad_engine::{
    AbsentScalars, CompileOptions, CorePlan, Deployed, Globals, Link, Placement, compile,
    import_layout,
};
use tetrad_preset::{Limits, PresetDocument};

use crate::preset_build::load_specs;
use crate::{Result, io, print_json};

#[derive(Subcommand)]
pub enum Command {
    /// Import, route, fit and compile a saved preset. Prints run lists and the fit result.
    Compile(Compile),
}

#[derive(Args)]
pub struct Compile {
    /// Saved preset file. Read only.
    #[arg(long)]
    input: PathBuf,
    /// Directory holding the decrypted ModelRepo.xml and moduleReport.xml.
    #[arg(long)]
    catalog: PathBuf,
}

pub fn run(command: Command) -> Result<bool> {
    match command {
        Command::Compile(args) => compile_preset(args),
    }
}

/// Uses the device path's import and global defaults. DDC is off until a flag needs it.
fn compile_preset(args: Compile) -> Result<bool> {
    let bytes = io::read_bounded(&args.input, Limits::default().max_bytes)?;
    let specs = load_specs(
        &args.catalog.join("ModelRepo.xml"),
        &args.catalog.join("moduleReport.xml"),
    )?;
    let summary = PresetDocument::parse(&bytes, Limits::default())?.summary()?;
    let imported = import_layout(&summary, &specs, AbsentScalars::Proto3Default)?;
    let options = CompileOptions {
        globals: Globals {
            input_gate: true,
            global_eq: true,
        },
        ddc: false,
    };
    let plan = compile(&imported.layout, &specs, options)?;
    print_json(&json!({
        "execution": "none; compile only",
        "hardware_compatibility": "unverified",
        "defaults": {
            "absent_scalars": "Proto3Default",
            "input_gate": true,
            "global_eq": true,
            "ddc": false,
        },
        "import": {
            "defaulted": imported.defaulted,
            "shadowed": imported.shadowed.len(),
            "padded": imported.padded.len(),
            "sanitized": imported.sanitized.len(),
            "ignored": imported.ignored,
            "migrated": imported.migrated.len(),
        },
        "cores": plan.cores.iter().map(describe_core).collect::<Vec<_>>(),
        "links": plan.links.iter().map(describe_link).collect::<Vec<_>>(),
        "sidechains": plan.sidechains.len(),
        "fit": {
            "masks": plan.assignment.masks,
            "gate_core": plan.gate_core.map(|core| core.index()),
            "inhibited": {
                "input_gate": plan.inhibited.input_gate,
                "global_eq": plan.inhibited.global_eq,
            },
        },
    }))?;
    Ok(true)
}

fn describe_core(core: &CorePlan) -> Value {
    json!({
        "core": core.core.index(),
        "usage": core.usage,
        "budget": core.budget,
        "run_list": core.slots.iter().map(describe_slot).collect::<Vec<_>>(),
    })
}

fn describe_slot(slot: &Deployed) -> Value {
    json!({
        "placement": describe_placement(slot.placement),
        "node": format!("{:?}", slot.node),
        "model": slot.model.0,
        "latency": slot.latency,
    })
}

fn describe_placement(placement: Placement) -> Value {
    json!({ "core": placement.core.index(), "slot": placement.slot.index() })
}

fn describe_link(link: &Link) -> Value {
    json!({
        "kind": format!("{:?}", link.kind),
        "from": describe_placement(link.from),
        "to": describe_placement(link.to),
        "channel": link.channel,
    })
}
