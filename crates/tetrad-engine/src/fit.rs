//! Stock resource fit and core assignment (`zenDSPCompiler::FitChecker`, `0x5f3940`).
//!
//! Each row pair is one group on one SOC: rows 0/1 use cores 0/1 (SOC1), rows 2/3 use cores 2/3
//! (SOC2). The spec builder (`0x5ed844`) turns a pair into an ordered item list. A candidate is a
//! bit mask over the items (bit set → core `2g+1`). Group 1 is searched before group 0
//! (`0x5f1330`): per level the candidates are generated (`0x5fde70`, `0x5fdaa0`), quick-scored on
//! item budgets (`0x5fe5c4`), pruned above 100 (`0x600300`) and fully scored with fixed routing
//! costs, slot and link-channel limits and the Input Gate / Global EQ placement (`0x601ac8`,
//! `0x5fc098`, `0x5fb108`). The best score wins; ties go to the first (`0x5fcd3c`).

use tetrad_catalog::{ModelSpec, ModuleStats, Specs, ids};

use crate::{COLUMNS, Column, Layout, ModelId, Pair, Row, Site, Slot};

/// A SHARC core, 0..=3. Cores 0/1 are SOC1 and 2/3 are SOC2; the UI's
/// "Core N" is core N−1. Matches `tetrad_dsp_protocol::DspCore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DspCore(u8);

impl DspCore {
    pub const COUNT: usize = 4;
    pub const ALL: [Self; 4] = [Self(0), Self(1), Self(2), Self(3)];
    /// The I/O hub: lane outputs, FX loops, the looper and the Global EQ run here.
    pub const IO: Self = Self(1);

    pub fn new(index: u8) -> Option<Self> {
        (usize::from(index) < Self::COUNT).then_some(Self(index))
    }

    pub fn index(self) -> usize {
        usize::from(self.0)
    }

    pub fn soc(self) -> Pair {
        if self.0 < 2 { Pair::Upper } else { Pair::Lower }
    }

    /// The two cores of a pair's SOC: `[2g, 2g+1]`.
    pub fn of(pair: Pair) -> [Self; 2] {
        let base = pair as u8 * 2;
        [Self(base), Self(base + 1)]
    }
}

/// Per-core budget template at `0x155e1f8` (`_INIT_514`).
pub const CORE_TEMPLATE: ModuleStats = ModuleStats {
    cpu: 96.0,
    dm_heap: 153_592,
    pm_heap: 114_680,
    sd_heap: 16 * 1024 * 1024,
    dmda: 59_392,
    pmda: 8_192,
    pmco: 130_672,
    edma: 85.0,
    singletons: 1,
    latency: 0,
    reuse_pmco: false,
    has_sidechain: false,
};

/// Per-SOC pre-check template at `0x155e11c`; no kernel or routing cost applies.
pub const SOC_TEMPLATE: ModuleStats = ModuleStats {
    cpu: 190.0,
    dm_heap: 307_184,
    pm_heap: 229_360,
    sd_heap: 32 * 1024 * 1024,
    dmda: 118_784,
    pmda: 16_384,
    pmco: 261_344,
    edma: 85.0,
    singletons: 1,
    latency: 0,
    reuse_pmco: false,
    has_sidechain: false,
};

const fn fixed(cpu: f32, dm_heap: u32, pm_heap: u32, sd_heap: u32, edma: f32) -> ModuleStats {
    ModuleStats {
        cpu,
        dm_heap,
        pm_heap,
        sd_heap,
        dmda: 0,
        pmda: 0,
        pmco: 0,
        edma,
        singletons: 0,
        latency: 0,
        reuse_pmco: true,
        has_sidechain: false,
    }
}

/// Fixed routing and global costs (`_INIT_514`, added by `0x5fb108`).
pub mod costs {
    use super::{ModuleStats, fixed};
    /// A: Input Gate, on core 0 or 1.
    pub const INPUT_GATE: ModuleStats = fixed(7.0, 0, 0, 0, 0.0);
    /// B: Global EQ, on core 1.
    pub const GLOBAL_EQ: ModuleStats = fixed(3.0, 0, 0, 0, 0.0);
    /// C: receive per lane input or cross-core input, on the item's core.
    pub const LINK_RECEIVE: ModuleStats = fixed(0.3343, 176, 0, 0, 0.0);
    /// D: core-1 endpoint per FX send off core 1 and per SOC2 return, loop or sidechain.
    pub const SOC2_ENDPOINT: ModuleStats = fixed(0.7833, 184, 0, 0, 0.0);
    /// E: core-1 transfer per SOC2 lane input, output off core 1 and empty-row output.
    pub const IO_TRANSFER: ModuleStats = fixed(0.9067, 624, 1808, 8456, 3.2);
    /// F: cross-core send, on the source core.
    pub const LINK_SEND: ModuleStats = fixed(0.336, 304, 0, 0, 0.0);
    /// G: lane output or FX send on core 1.
    pub const IO_PORT: ModuleStats = fixed(0.7297, 304, 0, 0, 0.0);
    /// H: per mixer input, output on core 1, FX return and sidechain.
    pub const MIX_PORT: ModuleStats = fixed(0.1003, 440, 1808, 8456, 3.2);
    /// I: Looper X (27000), on core 1.
    pub const LOOPER_X: ModuleStats = fixed(1.7, 0, 0, 0, 0.0);
}

/// What a fit item stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemNode {
    Block(Slot),
    Splitter(Pair),
    /// The 10003 head of a split path; `slave` selects the slave side.
    PathHead {
        pair: Pair,
        slave: bool,
    },
    Mixer(Pair),
}

impl ItemNode {
    pub fn site(self) -> Option<Site> {
        match self {
            Self::Block(slot) => Some(Site::Slot(slot)),
            Self::Splitter(pair) => Some(Site::Splitter(pair)),
            Self::Mixer(pair) => Some(Site::Mixer(pair)),
            Self::PathHead { .. } => None,
        }
    }
}

/// An item input: a lane's own input (stock id `-(row+1)`) or an earlier item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemInput {
    Lane(Row),
    Item(usize),
}

/// One spec-builder item (`0x5ed844`, 0x14 bytes in stock). Bit `i` of a
/// candidate mask belongs to item `i` (`0x5fd284` keeps it with the item).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub node: ItemNode,
    /// The model the DSP runs (a cab's `clones` engine for cabs).
    pub model: ModelId,
    /// The model whose stats are charged (cab ids are remapped). Fixed costs
    /// key on this id, as stock keys on item `+0xc`. An owned replacement
    /// (`tetrad_catalog::owned::replace`) puts its stats on the stock spec, so it is
    /// charged here like any stock model, without code reuse.
    pub stats_model: ModelId,
    pub stats: ModuleStats,
    pub inputs: Vec<ItemInput>,
    pub kind: ItemKind,
}

/// Items the pre-check counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Plain,
    FxLoop,
    Looper,
    Sidechain,
}

/// The items of one row pair and its lane outputs.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub pair: Pair,
    pub items: Vec<Item>,
    /// One entry per unmerged row, master first, whatever its output port
    /// (`0x5ed844` pushes them unconditionally): the row's last item, or its
    /// lane input when the row is empty.
    pub outputs: Vec<ItemInput>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum FitError {
    #[error("{node:?} model {model:?} has no resource report")]
    MissingStats { node: ItemNode, model: ModelId },
    #[error("only one Looper fits in a preset")]
    Loopers,
    #[error("{0:?} pair holds more than two sidechain models")]
    Sidechains(Pair),
    #[error("{pair:?} pair needs {usage:?}, above its SOC budget")]
    Soc { pair: Pair, usage: ModuleStats },
    #[error("{0:?} pair has no core assignment that fits")]
    Cores(Pair),
}

/// Device-wide features that consume DSP resources outside the preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Globals {
    pub input_gate: bool,
    pub global_eq: bool,
}

/// Global features the fit had to switch off (`CompilerInhibitedModules`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Inhibited {
    pub input_gate: bool,
    pub global_eq: bool,
}

/// A core assignment a later fit may keep (`0x5f3940` → `0x5efa08`).
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub groups: [Group; 2],
    /// Candidate mask per group: bit `i` set puts item `i` on core `2g+1`.
    pub masks: [u64; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    pub groups: [Group; 2],
    /// Core of every item, per group.
    pub cores: [Vec<DspCore>; 2],
    pub masks: [u64; 2],
    /// Selected score per group (`0x5fc098`).
    pub scores: [f32; 2],
    pub gate_core: Option<DspCore>,
    pub inhibited: Inhibited,
    /// Usage per core including fixed costs; core 0/1 include SOC2 overhead.
    pub usage: [ModuleStats; 4],
    pub budgets: [ModuleStats; 4],
    /// Run-list slots per core as the fit counts them (`0x5fb108`).
    pub slots: [u32; 4],
}

impl Fit {
    pub fn assignment(&self) -> Assignment {
        Assignment {
            groups: self.groups.clone(),
            masks: self.masks,
        }
    }
}

/// Budget per core: the template minus the kernel's CPU and heaps (`0x8826a4`).
pub fn budgets(specs: &Specs) -> [ModuleStats; 4] {
    std::array::from_fn(|core| {
        let mut budget = CORE_TEMPLATE;
        if let Some(kernel) = specs.kernels.get(&(core as u32)) {
            budget.cpu -= kernel.cpu;
            budget.dm_heap = budget.dm_heap.saturating_sub(kernel.dm_heap);
            budget.pm_heap = budget.pm_heap.saturating_sub(kernel.pm_heap);
            budget.sd_heap = budget.sd_heap.saturating_sub(kernel.sd_heap);
        }
        budget
    })
}

/// `0x5f97dc`: the largest resource ratio against the core template, capped at 1.
pub fn utilisation(usage: &ModuleStats) -> f32 {
    let t = CORE_TEMPLATE;
    let ratios = [
        usage.cpu / 100.0,
        usage.dm_heap as f32 / t.dm_heap as f32,
        usage.pm_heap as f32 / t.pm_heap as f32,
        usage.sd_heap as f32 / t.sd_heap as f32,
        usage.edma / 100.0,
        usage.pmco as f32 / t.pmco as f32,
        usage.pmda as f32 / t.pmda as f32,
        usage.dmda as f32 / t.dmda as f32,
    ];
    ratios.into_iter().fold(0.0, f32::max).min(1.0)
}

/// Builds the stock item list for one pair (`0x5ed844`). Rows run column 0..7,
/// master first. A split inserts the splitter and a 10003 path head before the
/// split column's block; the slave row starts with its own 10003 head. The mixer
/// follows the mix column's block and takes the slave row's last item as its
/// second input. Every unmerged row adds an output.
pub fn group(layout: &Layout, specs: &Specs, pair: Pair) -> Result<Group, FitError> {
    let (master, slave) = (pair.master(), pair.slave());
    let junctions = layout.pair(pair);
    let mut items = Vec::new();
    let mut cabs = CabRemap::default();
    let mut push = |items: &mut Vec<Item>, node: ItemNode, model: ModelId, inputs| {
        let item = item(specs, &mut cabs, node, model, inputs)?;
        items.push(item);
        Ok::<_, FitError>(ItemInput::Item(items.len() - 1))
    };
    let mut prev = ItemInput::Lane(master);
    let mut splitter = None;
    let mut mixer = None;
    for column in 0..COLUMNS {
        let at = |junction: Option<Column>| junction.is_some_and(|c| c.index() == column);
        if at(junctions.split)
            && let Some(instance) = &junctions.splitter
        {
            let split = push(
                &mut items,
                ItemNode::Splitter(pair),
                instance.model,
                vec![prev],
            )?;
            splitter = Some(split);
            let head = ItemNode::PathHead { pair, slave: false };
            prev = push(&mut items, head, ModelId(PATH_HEAD), vec![split])?;
        }
        let slot = Slot {
            row: master,
            column: Column(column as u8),
        };
        if let Some(instance) = layout.instance(Site::Slot(slot)) {
            prev = push(
                &mut items,
                ItemNode::Block(slot),
                instance.model,
                vec![prev],
            )?;
        }
        if at(junctions.merge)
            && let Some(instance) = &junctions.mixer
        {
            prev = push(
                &mut items,
                ItemNode::Mixer(pair),
                instance.model,
                vec![prev],
            )?;
            mixer = Some(items.len() - 1);
        }
    }
    let mut outputs = vec![prev];
    let mut slave_prev = match splitter {
        Some(split) => push(
            &mut items,
            ItemNode::PathHead { pair, slave: true },
            ModelId(PATH_HEAD),
            vec![split],
        )?,
        None => ItemInput::Lane(slave),
    };
    for column in 0..COLUMNS {
        let slot = Slot {
            row: slave,
            column: Column(column as u8),
        };
        if let Some(instance) = layout.instance(Site::Slot(slot)) {
            slave_prev = push(
                &mut items,
                ItemNode::Block(slot),
                instance.model,
                vec![slave_prev],
            )?;
        }
    }
    match mixer {
        Some(mixer) => items[mixer].inputs.push(slave_prev),
        None => outputs.push(slave_prev),
    }
    Ok(Group {
        pair,
        items,
        outputs,
    })
}

/// Stock model id of a split path head (LR Crossover).
pub const PATH_HEAD: u32 = 10_003;

/// Per-pair cab engine remap. The first mono cab of a pair runs the FIR engine
/// (12000, or 12115 for plugin 13, or 12100 for another plugin); later mono cabs
/// use the core engines 12014/12116/12114. Stereo cabs use 32000/32089/32100.
/// The core engines themselves keep their id and do not count as the first.
#[derive(Default)]
struct CabRemap {
    mono_seen: bool,
}

impl CabRemap {
    fn stats_model(&mut self, spec: &ModelSpec) -> u32 {
        let variant = |default, petrucci, plugin| match spec.plugin_id {
            None => default,
            Some(13) => petrucci,
            Some(_) => plugin,
        };
        match spec.category_id {
            12 | 21 if matches!(spec.id, 12_014 | 12_114 | 12_116) => spec.id,
            12 | 21 if !self.mono_seen => {
                self.mono_seen = true;
                variant(12_000, 12_115, 12_100)
            }
            12 | 21 => variant(12_014, 12_116, 12_114),
            32 | 33 => variant(32_000, 32_089, 32_100),
            _ => spec.id,
        }
    }
}

fn item(
    specs: &Specs,
    cabs: &mut CabRemap,
    node: ItemNode,
    model: ModelId,
    inputs: Vec<ItemInput>,
) -> Result<Item, FitError> {
    let spec = specs.model(model.0);
    let kind = match spec.map(|spec| spec.category_id) {
        Some(13) => ItemKind::FxLoop,
        Some(27) => ItemKind::Looper,
        _ if spec.is_some_and(|spec| spec.sidechain.is_some()) => ItemKind::Sidechain,
        _ => ItemKind::Plain,
    };
    let stats_model = spec.map_or(model.0, |spec| cabs.stats_model(spec));
    let stats = match specs.model(stats_model).and_then(|spec| spec.stats) {
        Some(stats) => stats,
        // Kernel-resident junctions and stat-less FX loops/loopers cost only
        // their fixed routing costs (stock looks up zeroed stats).
        None if matches!(node, ItemNode::Splitter(_) | ItemNode::Mixer(_))
            || matches!(kind, ItemKind::FxLoop | ItemKind::Looper) =>
        {
            ModuleStats {
                reuse_pmco: true,
                ..ModuleStats::default()
            }
        }
        // Stock skips other stat-less blocks (`0x5ed844`); refusing is owned.
        None => return Err(FitError::MissingStats { node, model }),
    };
    let deployed = spec
        .and_then(|spec| spec.clones.first().copied())
        .map_or(model, ModelId);
    Ok(Item {
        node,
        model: deployed,
        stats_model: ModelId(stats_model),
        stats,
        inputs,
        kind,
    })
}

/// Per group, the stored core mask the fit tries before it searches
/// (`0x5f3940` → `0x5efa08`; kept when it fits, whatever its score, unless it
/// drops the Global EQ). Stock keeps the mask in the preset (`+0x168c`) with a
/// dirty byte (`+0x1694`) that block edits of the group set (`0x2e07b8` and
/// the other block editors) and each fit stores again (`0x4e2428`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoredMasks {
    /// `None` means dirty: search.
    pub masks: [Option<u64>; 2],
    /// Preset word `+0x1698` (`recompile_code_k[0] != 0`): a stored mask may
    /// drop the Global EQ. When it is clear, `0x5efa08` rejects a stored mask
    /// whose placement does not charge the EQ (`0x5f03a8`..`0x5f03cc`) and
    /// searches; that rejection also sets the word and the dirty byte in the
    /// preset (`0x5f0ddc` → `0x4e2428`), which Tetrad does not store.
    pub may_drop_eq: [bool; 2],
}

impl StoredMasks {
    /// The load path (`0x681d44`). A group is clean with mask
    /// `layout_code_k[0]` when `layout_code_k` (fields 20/21, message `+0xa0`
    /// and `+0xb0`) holds exactly one entry; otherwise it is dirty. The first
    /// `recompile_code_k` entry (fields 29/30, `+0x130`/`+0x140`) is the EQ
    /// word. A new preset stores `[0]` in all four fields: every item on core 2g.
    pub fn from_preset(preset: &tetrad_preset::Preset) -> Self {
        let mask = |codes: &[u32]| match codes {
            [code] => Some(u64::from(*code)),
            _ => None,
        };
        let word = |codes: &[u32]| codes.first().is_some_and(|&code| code != 0);
        Self {
            masks: [mask(&preset.layout_code_1), mask(&preset.layout_code_2)],
            may_drop_eq: [
                word(&preset.recompile_code_1),
                word(&preset.recompile_code_2),
            ],
        }
    }
}

/// Runs the stock fit for the whole layout. `previous` is the assignment of
/// the deployed preset: a group whose items are unchanged keeps its mask when
/// that mask still fits (`0x5efa08`), so a small edit keeps the cores. Item
/// equality stands in for the stock dirty byte, and the EQ word is taken as
/// clear (both inferred).
pub fn fit(
    layout: &Layout,
    specs: &Specs,
    globals: Globals,
    previous: Option<&Assignment>,
) -> Result<Fit, FitError> {
    let groups = [
        group(layout, specs, Pair::Upper)?,
        group(layout, specs, Pair::Lower)?,
    ];
    let stored = StoredMasks {
        masks: std::array::from_fn(|g| {
            previous
                .filter(|previous| previous.groups[g] == groups[g])
                .map(|previous| previous.masks[g])
        }),
        may_drop_eq: [false; 2],
    };
    fit_groups(groups, specs, globals, stored)
}

/// [`fit`] with masks a preset stores ([`StoredMasks`]).
pub fn fit_stored(
    layout: &Layout,
    specs: &Specs,
    globals: Globals,
    stored: StoredMasks,
) -> Result<Fit, FitError> {
    let groups = [
        group(layout, specs, Pair::Upper)?,
        group(layout, specs, Pair::Lower)?,
    ];
    fit_groups(groups, specs, globals, stored)
}

fn fit_groups(
    groups: [Group; 2],
    specs: &Specs,
    globals: Globals,
    stored: StoredMasks,
) -> Result<Fit, FitError> {
    precheck(&groups)?;
    let budgets = budgets(specs);
    // Bit i belongs to item i; higher bits are ignored.
    let kept = |g: usize| {
        let width = groups[g].items.len();
        let mask = stored.masks[g]
            .map(|mask| mask & 1u64.checked_shl(width as u32).map_or(u64::MAX, |m| m - 1));
        mask.map(|mask| Stored {
            mask,
            may_drop_eq: stored.may_drop_eq[g],
        })
    };
    // Group 1 first; its core-0/1 overhead and best score feed group 0.
    let lower = Search {
        group: &groups[1],
        budgets: &budgets,
        globals,
        overhead: Overhead::default(),
        soc2_used: false,
    };
    let lower_fit = lower
        .run(kept(1), -2.0)
        .ok_or(FitError::Cores(Pair::Lower))?;
    let soc2_used = !groups[1].items.is_empty();
    let upper = Search {
        group: &groups[0],
        budgets: &budgets,
        globals,
        overhead: lower_fit.eval.overhead,
        soc2_used,
    };
    let base = if soc2_used {
        lower_fit.eval.score.min(-2.0)
    } else {
        -1.0
    };
    let upper_fit = upper
        .run(kept(0), base)
        .ok_or(FitError::Cores(Pair::Upper))?;
    let (upper_eval, lower_eval) = (&upper_fit.eval, &lower_fit.eval);
    let usage = [
        upper_eval.usage[0],
        upper_eval.usage[1],
        lower_eval.usage[0],
        lower_eval.usage[1],
    ];
    let slots = [
        upper_eval.slots[0],
        upper_eval.slots[1],
        lower_eval.slots[2],
        lower_eval.slots[3],
    ];
    let masks = [upper_fit.mask, lower_fit.mask];
    let cores = [
        assignment(&groups[0], masks[0]),
        assignment(&groups[1], masks[1]),
    ];
    Ok(Fit {
        cores,
        masks,
        scores: [upper_eval.score, lower_eval.score],
        gate_core: upper_eval.gate,
        inhibited: Inhibited {
            input_gate: globals.input_gate && upper_eval.gate.is_none(),
            global_eq: globals.global_eq && !upper_eval.eq,
        },
        usage,
        budgets,
        slots,
        groups,
    })
}

fn assignment(group: &Group, mask: u64) -> Vec<DspCore> {
    (0..group.items.len())
        .map(|item| core_of(group.pair, mask, item))
        .collect()
}

fn core_of(pair: Pair, mask: u64, item: usize) -> DspCore {
    DspCore::of(pair)[(mask >> item & 1) as usize]
}

/// `0x5f7b7c`: at most one Looper per preset, two sidechain models per group,
/// and each group within the SOC template.
fn precheck(groups: &[Group; 2]) -> Result<(), FitError> {
    let loopers = groups
        .iter()
        .flat_map(|group| &group.items)
        .filter(|item| item.kind == ItemKind::Looper)
        .count();
    if loopers > 1 {
        return Err(FitError::Loopers);
    }
    for group in groups {
        let sidechains = group
            .items
            .iter()
            .filter(|item| item.stats.has_sidechain)
            .count();
        if sidechains > 2 {
            return Err(FitError::Sidechains(group.pair));
        }
        let mut total = ModuleStats::default();
        for (index, item) in group.items.iter().enumerate() {
            let shared = item.stats.reuse_pmco
                && group.items[..index]
                    .iter()
                    .any(|earlier| earlier.stats_model == item.stats_model);
            total.add(&item.stats, shared);
        }
        // Each core holds one FIR engine, so a SOC holds two.
        total.singletons = total.singletons.saturating_sub(1);
        if !total.fits(&SOC_TEMPLATE) {
            return Err(FitError::Soc {
                pair: group.pair,
                usage: total,
            });
        }
    }
    Ok(())
}

/// Score of an infeasible candidate.
const INFEASIBLE: f32 = -100.0;
/// Deepest level: 0 = one core, 1..=3 = that many core changes, 4 = more.
/// The level count field of the FitChecker is not recovered (inferred 5).
const LEVELS: usize = 5;
/// Above this many candidates the quick scores are pruned (`0x600300`).
const PRUNE_ABOVE: usize = 100;
/// Run-list slots per core and Dual Core channels per SOC the fit admits.
const MAX_SLOTS: u32 = 30;
const SOC1_DUAL_CHANNELS: u32 = 24;
const SOC2_DUAL_CHANNELS: u32 = 12;
const SLAVE_LINK_CHANNELS: u32 = 8;

/// SOC1 costs and slots that the group-1 solution leaves on cores 0/1
/// (fit-checker `+0x8c`, `+0xb8`, `+0xe4`, `+0xe8`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Overhead {
    usage: [ModuleStats; 2],
    slots: [u32; 2],
}

/// A fully evaluated candidate (`0x5fc098` with `0x5fb108`).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Eval {
    score: f32,
    gate: Option<DspCore>,
    /// The Global EQ is charged; stock flag `+0x54` is its negation.
    eq: bool,
    /// Per side of the group, with fixed costs (and group-1 overhead on SOC1).
    usage: [ModuleStats; 2],
    /// Group 1 only: what it adds to cores 0/1.
    overhead: Overhead,
    /// Slots per global core (group 0 includes the group-1 overhead slots).
    slots: [u32; 4],
}

impl Eval {
    fn flagged(&self) -> bool {
        !self.eq
    }
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    mask: u64,
    eval: Eval,
}

/// One group's clean stored mask ([`StoredMasks`]).
#[derive(Debug, Clone, Copy)]
struct Stored {
    mask: u64,
    may_drop_eq: bool,
}

/// One group's search (`0x5f1330`).
struct Search<'a> {
    group: &'a Group,
    budgets: &'a [ModuleStats; 4],
    globals: Globals,
    overhead: Overhead,
    /// Group 0 only: SOC2 holds items, so levels 2–4 keep the base threshold.
    soc2_used: bool,
}

impl Search<'_> {
    fn base(&self) -> usize {
        self.group.pair as usize * 2
    }

    fn core(&self, mask: u64, item: usize) -> usize {
        self.base() + (mask >> item & 1) as usize
    }

    /// Stored mask first (`0x5efa08`), then the level search from base
    /// threshold `base`. A stored mask whose placement does not charge the
    /// Global EQ is rejected unless the group's EQ word is set (`0x5f03a8`).
    /// Stock also accepts it when preset byte `+0x16c0` is set; that byte is
    /// not decoded and is taken as clear (inferred).
    fn run(&self, stored: Option<Stored>, base: f32) -> Option<Candidate> {
        if let Some(Stored { mask, may_drop_eq }) = stored
            && let Some(eval) = self.place(mask)
            && (may_drop_eq || !eval.flagged())
        {
            return Some(Candidate { mask, eval });
        }
        self.levels(base)
    }

    /// The level loop of `0x5f1330`. Ties of the best score carry to the next
    /// level at the front; the result is the first tie.
    fn levels(&self, base: f32) -> Option<Candidate> {
        let upper = self.group.pair == Pair::Upper;
        let n = self.group.items.len();
        let (mut best, mut worst) = (INFEASIBLE, 100.0f32);
        let mut ties: Vec<Candidate> = Vec::new();
        for level in 0..LEVELS {
            let threshold = match level {
                0 => base,
                1 => base - 1.0,
                _ if self.soc2_used => base,
                _ => base - 1.0,
            };
            let mut masks = if level < 4 {
                masks(n, level, upper)
            } else {
                separated_masks(n)
            };
            if n == 0 {
                // `0x5f1330` adds the empty candidate at every level.
                masks.push(0);
            }
            let mut quick: Vec<(u64, f32)> = masks
                .into_iter()
                .filter_map(|mask| self.quick(mask).map(|score| (mask, score)))
                .collect();
            // No new candidate: no pass and no acceptance; ties stay carried.
            if quick.is_empty() {
                continue;
            }
            if quick.len() > PRUNE_ABOVE {
                let cut = (worst + (best - worst) * 0.75).floor();
                quick.retain(|&(_, score)| score >= cut);
            }
            // Full pass: carried ties first; stop after the first candidate at
            // or above the pass threshold that charges the EQ (`0x601ac8`).
            let pass = if level < 2 { 0.0 } else { threshold };
            let mut list = Vec::new();
            let mut found = false;
            let carried = std::mem::take(&mut ties).into_iter().map(Ok);
            let fresh = quick.into_iter().map(|(mask, _)| Err(mask));
            for entry in carried.chain(fresh) {
                if found {
                    break;
                }
                let candidate = match entry {
                    Ok(candidate) => candidate,
                    Err(mask) => match self.place(mask) {
                        Some(eval) => Candidate { mask, eval },
                        None => continue,
                    },
                };
                let score = candidate.eval.score;
                best = best.max(score);
                worst = worst.min(score);
                found = score >= pass && !candidate.eval.flagged();
                list.push(candidate);
            }
            // Best score; only EQ-charging candidates count when both kinds exist.
            let mixed =
                list.iter().any(|c| c.eval.flagged()) && list.iter().any(|c| !c.eval.flagged());
            if mixed {
                best = list
                    .iter()
                    .filter(|c| !c.eval.flagged())
                    .map(|c| c.eval.score)
                    .fold(INFEASIBLE, f32::max);
            }
            ties = list
                .into_iter()
                .filter(|c| (!mixed || !c.eval.flagged()) && c.eval.score == best)
                .collect();
            if ties.is_empty() {
                continue;
            }
            let mut accepted = threshold <= best || (level > 2 && threshold <= best + 2.0);
            if upper && level < 4 && ties.iter().all(|c| c.eval.flagged()) {
                accepted = false;
            }
            // Any solution found by level 3 is used (loop end at level 4).
            if accepted || level >= 3 {
                return ties.first().copied();
            }
        }
        ties.first().copied()
    }

    /// Quick pass (`0x5fe5c4`): items only, gate unplaced, no balance bonus.
    fn quick(&self, mask: u64) -> Option<f32> {
        self.items(mask)?;
        Some(self.connections(mask, None) as f32)
    }

    /// Per-side item usage with code sharing; `None` over budget.
    fn items(&self, mask: u64) -> Option<[ModuleStats; 2]> {
        let mut usage = [ModuleStats::default(); 2];
        let mut models: [Vec<ModelId>; 2] = Default::default();
        for (index, item) in self.group.items.iter().enumerate() {
            let side = (mask >> index & 1) as usize;
            let shared = item.stats.reuse_pmco && models[side].contains(&item.stats_model);
            usage[side].add(&item.stats, shared);
            models[side].push(item.stats_model);
        }
        let fits = (0..2).all(|side| usage[side].fits(&self.budgets[self.base() + side]));
        fits.then_some(usage)
    }

    /// Connection penalties of `0x5fc098`: −1 per input on another core (a lane
    /// input counts as on the gate core, never on SOC2); group 0: −1 per output
    /// off core 1 (a lane pass-through counts as on the gate core); group 1: −1.
    fn connections(&self, mask: u64, gate: Option<DspCore>) -> i32 {
        let gate = gate.map(DspCore::index);
        let mut score = 0;
        for (index, item) in self.group.items.iter().enumerate() {
            let core = self.core(mask, index);
            for input in &item.inputs {
                let source = match *input {
                    ItemInput::Lane(_) if core > 1 => None,
                    ItemInput::Lane(_) => gate,
                    ItemInput::Item(source) => Some(self.core(mask, source)),
                };
                if source != Some(core) {
                    score -= 1;
                }
            }
        }
        if self.group.pair == Pair::Lower {
            return score - 1;
        }
        for output in &self.group.outputs {
            let core = match *output {
                ItemInput::Item(item) => Some(self.core(mask, item)),
                ItemInput::Lane(_) => gate,
            };
            if core != Some(DspCore::IO.index()) {
                score -= 1;
            }
        }
        score
    }

    /// Full evaluation with the stock global placement order (`0x601ac8`):
    /// gate@1+EQ, gate@0+EQ, then (EQ enabled only) gate@1, gate@0, neither.
    /// Group 1 places nothing. The first feasible placement is the candidate's.
    fn place(&self, mask: u64) -> Option<Eval> {
        let Globals {
            input_gate,
            global_eq,
        } = self.globals;
        let gates = if self.group.pair == Pair::Upper && input_gate {
            vec![Some(DspCore::IO), Some(DspCore::ALL[0])]
        } else {
            vec![None]
        };
        let mut tries: Vec<(Option<DspCore>, bool)> =
            gates.iter().map(|&gate| (gate, global_eq)).collect();
        if global_eq {
            tries.extend(gates.iter().map(|&gate| (gate, false)));
            tries.push((None, false));
        }
        tries
            .into_iter()
            .find_map(|(gate, eq)| self.evaluate(mask, gate, eq))
    }

    /// `0x5fc098` full pass with `0x5fb108`; `None` when infeasible.
    fn evaluate(&self, mask: u64, gate: Option<DspCore>, eq: bool) -> Option<Eval> {
        let items = self.items(mask)?;
        let costs = self.costs(mask, gate, eq)?;
        let upper = self.group.pair == Pair::Upper;
        let mut usage = items;
        let mut slots = costs.slots;
        let mut overhead = Overhead::default();
        for side in 0..2 {
            if upper {
                usage[side].add(&costs.usage[side], false);
                usage[side].add(&self.overhead.usage[side], false);
                slots[side] += self.overhead.slots[side];
            } else {
                usage[side].add(&costs.usage[2 + side], false);
                overhead.usage[side] = costs.usage[side];
                overhead.slots[side] = costs.slots[side];
            }
        }
        let limits = if upper {
            slots[0] <= MAX_SLOTS && slots[1] <= MAX_SLOTS && costs.dual <= SOC1_DUAL_CHANNELS
        } else {
            costs.slave <= SLAVE_LINK_CHANNELS
                && slots[2] <= MAX_SLOTS
                && slots[3] <= MAX_SLOTS
                && costs.dual <= SOC2_DUAL_CHANNELS
        };
        let fits = (0..2).all(|side| usage[side].fits(&self.budgets[self.base() + side]));
        if !(limits && fits) {
            return None;
        }
        let (u0, u1) = (utilisation(&usage[0]), utilisation(&usage[1]));
        let bonus = if u0 + u1 > 0.0 {
            (1.0 - (u1 - u0).abs() / (u0 + u1)) * 0.9
        } else {
            0.0
        };
        Some(Eval {
            score: bonus + self.connections(mask, gate) as f32,
            gate,
            eq,
            usage,
            overhead,
            slots,
        })
    }

    /// Fixed routing costs, slots and link channels of one candidate
    /// (`0x5fb108`), per global core.
    fn costs(&self, mask: u64, gate: Option<DspCore>, eq: bool) -> Option<Costs> {
        let upper = self.group.pair == Pair::Upper;
        let mut c = Costs::default();
        if upper {
            // The globals run in kernel slots: no run-list slot.
            if let Some(gate) = gate {
                c.usage[gate.index()].add(&costs::INPUT_GATE, false);
            }
            if eq {
                c.usage[1].add(&costs::GLOBAL_EQ, false);
            }
        }
        let io = DspCore::IO.index();
        for (index, item) in self.group.items.iter().enumerate() {
            let core = self.core(mask, index);
            c.slots[core] += 1;
            let model = item.stats_model.0;
            for (position, input) in item.inputs.iter().enumerate() {
                let source = match *input {
                    ItemInput::Lane(_) => {
                        if !upper {
                            c.charge(io, &costs::IO_TRANSFER);
                        }
                        c.charge(core, &costs::LINK_RECEIVE);
                        if model == ids::MIXER {
                            c.charge(core, &costs::MIX_PORT);
                            if position & 1 == 0 {
                                c.charge(core, &costs::LINK_RECEIVE);
                            }
                        }
                        None
                    }
                    ItemInput::Item(source) => {
                        let source = self.core(mask, source);
                        if model == ids::MIXER {
                            if position == 0 {
                                c.charge(core, &costs::MIX_PORT);
                                c.charge(core, &costs::LINK_RECEIVE);
                            } else {
                                c.charge(source, &costs::MIX_PORT);
                                if source == core {
                                    c.charge(core, &costs::LINK_SEND);
                                }
                            }
                        }
                        if source != core {
                            c.charge(source, &costs::LINK_SEND);
                            c.charge(core, &costs::LINK_RECEIVE);
                            c.dual += 2;
                        }
                        Some(source)
                    }
                };
                c.model_costs(model, core, source, upper);
                if item.stats.has_sidechain {
                    if !matches!(model, 16_008 | 16_013 | 30_001 | 18_012) {
                        c.charge(core, &costs::LINK_RECEIVE);
                    }
                    c.charge(core, &costs::MIX_PORT);
                    if !upper {
                        c.charge(io, &costs::SOC2_ENDPOINT);
                        c.slave += 1;
                    }
                }
            }
        }
        for output in &self.group.outputs {
            match *output {
                ItemInput::Lane(_) => c.charge(io, &costs::IO_TRANSFER),
                ItemInput::Item(item) => match self.core(mask, item) {
                    core if core == io => {
                        c.charge(io, &costs::MIX_PORT);
                        c.charge(io, &costs::IO_PORT);
                    }
                    core => {
                        c.charge(core, &costs::LINK_SEND);
                        c.charge(io, &costs::IO_TRANSFER);
                        if upper {
                            c.dual += 2;
                        }
                    }
                },
            }
        }
        Some(c)
    }
}

/// Accumulated `0x5fb108` state: costs and slots per global core, Dual Core
/// channels (`local_11c`) and SlaveLink channels (`local_124`, base 4).
#[derive(Debug, Clone, Copy)]
struct Costs {
    usage: [ModuleStats; 4],
    slots: [u32; 4],
    dual: u32,
    slave: u32,
}

impl Default for Costs {
    fn default() -> Self {
        Self {
            usage: [ModuleStats::default(); 4],
            slots: [0; 4],
            dual: 0,
            slave: 4,
        }
    }
}

impl Costs {
    /// Adds one routing cost; each takes a run-list slot.
    fn charge(&mut self, core: usize, cost: &ModuleStats) {
        self.usage[core].add(cost, false);
        self.slots[core] += 1;
    }

    /// Per-model costs: Splitter (10004), FX loops (13000–13008), Looper X.
    fn model_costs(&mut self, model: u32, core: usize, source: Option<usize>, upper: bool) {
        let io = DspCore::IO.index();
        // A return (13002/13003/13007) or loop (13004/13005/13008) on SOC2
        // takes a core-1 endpoint and SlaveLink channels.
        let soc2_endpoint = |c: &mut Self, stereo: bool| {
            if !upper {
                c.charge(io, &costs::SOC2_ENDPOINT);
                c.slave += if stereo { 2 } else { 1 };
            }
        };
        // A send (13000/13001/13006, and the loops) leaves from core 1.
        let send = |c: &mut Self| {
            if core == io {
                c.charge(io, &costs::IO_PORT);
            } else {
                c.charge(core, &costs::LINK_SEND);
                c.charge(io, &costs::SOC2_ENDPOINT);
                c.dual += 2;
            }
        };
        match model {
            ids::SPLITTER => match source {
                Some(source) if source == core => self.charge(core, &costs::LINK_SEND),
                Some(_) => {}
                None => self.charge(core, &costs::LINK_RECEIVE),
            },
            13_000 | 13_001 | 13_006 => send(self),
            13_002 | 13_003 | 13_007 => {
                self.charge(core, &costs::LINK_RECEIVE);
                self.charge(core, &costs::MIX_PORT);
                soc2_endpoint(self, model == 13_007);
            }
            13_004 | 13_005 | 13_008 => {
                self.charge(core, &costs::LINK_RECEIVE);
                self.charge(core, &costs::MIX_PORT);
                soc2_endpoint(self, model == 13_008);
                send(self);
            }
            27_000 => {
                self.charge(core, &costs::LINK_SEND);
                self.charge(core, &costs::LINK_RECEIVE);
                self.charge(io, &costs::LOOPER_X);
            }
            _ => {}
        }
    }
}

/// Candidate masks of levels 0–3 (`0x5fde70`), in stock order. Bit `i` set puts
/// item `i` on core `2g+1`. Group 0 follows each mask with its complement.
fn masks(n: usize, level: usize, complements: bool) -> Vec<u64> {
    let ones = |bits: usize| (1u64 << bits) - 1;
    let all = ones(n);
    let mut out = Vec::new();
    if n == 0 {
        return out;
    }
    let mut push = |mask: u64| {
        out.push(mask);
        if complements {
            out.push(all - mask);
        }
    };
    match level {
        0 => return vec![all],
        1 => (1..n).for_each(|k| push(ones(k))),
        2 => {
            for s in 1..n.saturating_sub(1) {
                for l in 1..n - s {
                    push(ones(l) << s);
                }
            }
        }
        3 => {
            for a in 1..n {
                for c in a + 1..n.saturating_sub(1) {
                    for l in 1..n - c {
                        push(ones(a) | ones(l) << c);
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// Level-4 masks (`0x5fdaa0`): two or three separated blocks on core `2g+1`
/// with more than three core changes, no complements.
fn separated_masks(n: usize) -> Vec<u64> {
    let ones = |bits: usize| (1u64 << bits) - 1;
    let changes = |mask: u64| {
        (1..n)
            .filter(|&i| (mask >> i & 1) != (mask >> (i - 1) & 1))
            .count()
    };
    let mut out = Vec::new();
    for h1 in 1..=n {
        for s1 in 0..=n - h1 {
            let e1 = s1 + h1;
            for h2 in 1..=n {
                for s2 in e1 + 1..=n.saturating_sub(h2) {
                    let two = ones(h1) << s1 | ones(h2) << s2;
                    if changes(two) > 3 {
                        out.push(two);
                    }
                    let e2 = s2 + h2;
                    for h3 in 1..=n {
                        for s3 in e2 + 1..=n.saturating_sub(h3) {
                            let three = two | ones(h3) << s3;
                            if changes(three) > 3 {
                                out.push(three);
                            }
                        }
                    }
                }
            }
        }
    }
    out
}
