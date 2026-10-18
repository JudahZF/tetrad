//! SOC1 boot plan for a loadsharc ELF and two kernel LDRs. Plans only: no
//! core control, mailbox, or memory access happens here.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};
use tetrad_loader::booter::StockBooter;
use tetrad_loader::plan::{BootPlan, Start, Step, Variant, plan_soc1};
use tetrad_loader::{Limits, RawLdr};

use crate::{Result, io, print_json};

const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Subcommand)]
pub enum Command {
    /// Print the SOC1 boot steps for the given images. Executes nothing.
    Plan(Plan),
}

#[derive(Args)]
pub struct Plan {
    /// Stock loadsharc ELF holding both booter tables.
    #[arg(long)]
    loadsharc: PathBuf,
    /// Raw ADI LDR kernel for core 1 (signature 0xac).
    #[arg(long)]
    core1: PathBuf,
    /// Raw ADI LDR kernel for core 2 (signature 0xab).
    #[arg(long)]
    core2: PathBuf,
    #[arg(long, value_enum, default_value_t = VariantArg::QuadCortex)]
    variant: VariantArg,
    /// Plan the restart path, which quiesces and stops both cores first.
    #[arg(long)]
    restart: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum VariantArg {
    QuadCortex,
    Atma,
}

impl From<VariantArg> for Variant {
    fn from(arg: VariantArg) -> Self {
        match arg {
            VariantArg::QuadCortex => Self::QuadCortex,
            VariantArg::Atma => Self::Atma,
        }
    }
}

pub fn run(command: Command) -> Result<bool> {
    match command {
        Command::Plan(args) => plan(args),
    }
}

fn plan(args: Plan) -> Result<bool> {
    let loadsharc_bytes = io::read_bounded(&args.loadsharc, MAX_IMAGE_BYTES)?;
    let core1_bytes = io::read_bounded(&args.core1, MAX_IMAGE_BYTES)?;
    let core2_bytes = io::read_bounded(&args.core2, MAX_IMAGE_BYTES)?;
    let booter = StockBooter::parse(&loadsharc_bytes)?;
    let core1 = RawLdr::parse(&core1_bytes, Limits::default())?;
    let core2 = RawLdr::parse(&core2_bytes, Limits::default())?;
    let variant = Variant::from(args.variant);
    let start = if args.restart {
        Start::Restart
    } else {
        Start::First
    };
    let plan = plan_soc1(&booter, &core1, &core2, variant, start)?;
    print_json(&json!({
        "execution": "none; plan only",
        "variant": format!("{variant:?}"),
        "start": format!("{start:?}"),
        "phases": {
            "quiesce": describe_phase(&plan.quiesce),
            "core2": describe_phase(&plan.core2),
            "core1": describe_phase(&plan.core1),
        }
    }))?;
    Ok(true)
}

fn describe_phase(plan: &BootPlan<'_>) -> Vec<Value> {
    plan.steps.iter().map(describe_step).collect()
}

/// Kinds, addresses, and lengths only. Register values and argument bytes are not printed.
fn describe_step(step: &Step<'_>) -> Value {
    match step {
        Step::Core { core, action } => json!({
            "kind": "core",
            "core": format!("{core:?}"),
            "action": format!("{action:?}"),
        }),
        Step::Write32 { address, .. } => json!({ "kind": "write32", "address": address.0 }),
        Step::Write16 { address, .. } => json!({ "kind": "write16", "address": address.0 }),
        Step::Delay(duration) => json!({ "kind": "delay", "millis": duration.as_millis() }),
        Step::Upload {
            address,
            swap,
            source,
        } => json!({
            "kind": "upload",
            "address": address.0,
            "swap": format!("{swap:?}"),
            "length": source.len(),
        }),
        Step::Stream {
            core,
            mailbox,
            image,
        } => json!({
            "kind": "stream",
            "core": format!("{core:?}"),
            "buffer": mailbox.buffer.0,
            "length": image.len(),
        }),
        Step::Arguments {
            core,
            mailbox,
            arguments,
        } => json!({
            "kind": "arguments",
            "core": format!("{core:?}"),
            "address": mailbox.data_to_arm.0,
            "length": arguments.len(),
        }),
    }
}
