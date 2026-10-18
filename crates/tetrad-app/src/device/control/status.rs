//! The subscribed status senders: IOMeter (5, `sendIOMeters` `0x6e0040` → `0x111cf8`), CPULoad (26,
//! `0x6e0064` → `0x14a7b4`) and Diagnostics (7, `0x23e6d4`); the ModuleStats (35) READ
//! (`0x19c434`).

use std::collections::{BTreeMap, BTreeSet};

use tetrad_catalog::{ModuleStats, Specs, ids};
use tetrad_control::codecs::{
    CPUChainLoad, CPUColumnLoad, CPULoadMessage, DSPCommsDiagnosticsMessage, DiagnosticsMessage,
    IOMeterMessage, MessageAction, ModuleStatsItem, ModuleStatsMessage, ModuleStatsSizeRelation,
    ModuleStatsSizeRelationPair, ModuleStatsValidModelId, SOC2ARMCommsDiagnosticsMessage,
};
use tetrad_dsp_protocol::responses::{DiagnosticsBlock, Soc2ArmDiagnostics};
use tetrad_engine::{ItemNode, budgets, utilisation};
use tetrad_ui::IoMeters;

use super::super::{Current, State};

/// `0x111cf8`: every field, as the bar level of its reading (`(max(dB, −40) + 40) / 40`, the
/// law of [`IoMeters::level`]; the limiters as `IoMeters` keeps them, minus the gain
/// reduction). The lane fields 36..41 are the headphones feed fold of `0x4021`
/// (`0x493464`). `hp_limiter_active` (19) is flag `+0x40` of `0x4936cc`: `−40 < min(0,
/// max(−40, −v))` for value 6 of `0x4022`, so it is set unless the headphone limiter takes 40
/// dB or more (S7 capture: 1 at idle, C). The device does not keep value 6, so the flag is
/// the no-reduction value (I).
pub(super) fn io_meters(state: &State) -> IOMeterMessage {
    let meters = &state.io_meters;
    let level = |db: f32| Some(IoMeters::level(db));
    let [xlr_1, xlr_2, out_3, out_4] = meters.outputs.map(level);
    let [xlr_1_limiter, xlr_2_limiter, out_3_limiter, out_4_limiter] = meters.limiters.map(level);
    let [
        [usb_output_1l, usb_output_1r],
        [usb_output_2l, usb_output_2r],
        [usb_output_3l, usb_output_3r],
        [usb_output_4l, usb_output_4r],
    ] = meters.usb_out.map(|pair| pair.map(level));
    let [
        [usb_input_1l, usb_input_1r],
        [usb_input_2l, usb_input_2r],
        [usb_input_3l, usb_input_3r],
        [usb_input_4l, usb_input_4r],
    ] = meters.usb_in.map(|pair| pair.map(level));
    let [
        grid_xlr_1,
        grid_xlr_2,
        grid_out_3,
        grid_out_4,
        grid_send_1,
        grid_send_2,
    ] = meters.hp_feed.map(level);
    IOMeterMessage {
        action: Some(MessageAction::Update.into()),
        input_1: level(meters.inputs[0]),
        input_2: level(meters.inputs[1]),
        return_1: level(meters.returns[0]),
        return_2: level(meters.returns[1]),
        xlr_1,
        xlr_1_limiter,
        xlr_2,
        xlr_2_limiter,
        out_3,
        out_3_limiter,
        out_4,
        out_4_limiter,
        send_1: level(meters.sends[0]),
        send_2: level(meters.sends[1]),
        hp_l: level(meters.headphones[0]),
        hp_r: level(meters.headphones[1]),
        usb_output_1l,
        usb_output_1r,
        usb_output_2l,
        usb_output_2r,
        usb_output_3l,
        usb_output_3r,
        usb_output_4l,
        usb_output_4r,
        usb_input_1l,
        usb_input_1r,
        usb_input_2l,
        usb_input_2r,
        usb_input_3l,
        usb_input_3r,
        usb_input_4l,
        usb_input_4r,
        grid_xlr_1,
        grid_xlr_2,
        grid_out_3,
        grid_out_4,
        grid_send_1,
        grid_send_2,
        hp_limiter_active: Some(true),
        ..Default::default()
    }
}

/// `0x14a7b4`: four chains (grid rows) of eight columns. A cell holds the load the compile
/// stored for its block (`0x512480` → `0x581600` → `0x5f97dc`, a fraction 0..1; empty 0) and
/// whether its item runs on the second core of its pair (`0x581804`; here the fit mask: bit
/// `i` puts item `i` on core `2g + 1`, I). Inferred: the charged item stats stand for the
/// model's stats (stock substitutes some categories and sums 27000 itself). `cpu_total_load`
/// (3) is the DSP CPU monitor mean (`0x364ba0` `+0x1c8`); 0 before the first DSP Diagnostics
/// reply (I: the monitor starts at 0).
pub(super) fn cpu_load(state: &State) -> CPULoadMessage {
    let mut table: [[CPUColumnLoad; tetrad_preset::COLUMNS]; tetrad_preset::ROWS] =
        std::array::from_fn(|_| {
            std::array::from_fn(|_| CPUColumnLoad {
                cpu_load: Some(0.0),
                is_on_core2: Some(false),
            })
        });
    if let Some(current) = &*state.current {
        let assignment = &current.deployment.plan().assignment;
        for (group, mask) in assignment.groups.iter().zip(assignment.masks) {
            for (index, item) in group.items.iter().enumerate() {
                let ItemNode::Block(slot) = item.node else {
                    continue;
                };
                table[slot.row.index()][slot.column.index()] = CPUColumnLoad {
                    cpu_load: Some(utilisation(&item.stats)),
                    is_on_core2: Some(mask >> index & 1 == 1),
                };
            }
        }
    }
    CPULoadMessage {
        action: Some(MessageAction::Update.into()),
        cpu_total_load: Some(state.cpu.total().unwrap_or(0.0)),
        chains: table
            .into_iter()
            .map(|columns| CPUChainLoad {
                columns: columns.to_vec(),
            })
            .collect(),
        ..Default::default()
    }
}

/// What `sendDiagnostics` (`0x23e6d4`) reads: the last DSP diagnostics block per core (DSP
/// response `0x00`, slot `0x6dfe28`), the last SOC2 ARM record (response `0x08`, copied in
/// `0x6e2b8c`) and the HID TX failure count (`+0x260`, `0x6dfbf8`). `hid_tx_sent` (`+0x3f8`)
/// feeds the USB Statistics page only.
#[derive(Debug, Default)]
pub struct Diagnostics {
    pub cores: [Option<DiagnosticsBlock>; 4],
    pub soc2: Option<Soc2ArmDiagnostics>,
    pub hid_tx_failures: u64,
    pub hid_tx_sent: u32,
}

#[cfg(test)]
impl Diagnostics {
    /// Nothing received yet.
    pub const NONE: Self = Self {
        cores: [None; 4],
        soc2: None,
        hid_tx_failures: 0,
        hid_tx_sent: 0,
    };
}

/// Stock serializers skip zero values (proto3).
pub(super) fn nonzero<T: Default + PartialEq>(value: T) -> Option<T> {
    (value != T::default()).then_some(value)
}

/// Diagnostics (7), sent every 1000 ms while subscribed (`0x23e6d4`): fields 3..7 always,
/// field 8 the HID TX failure count (named `soc1_to_soc2_dropped_count`). Per core: words
/// 0..3 status, 5 CPU and 9 CPU max and 10 DMA as raw `f32` bits, 6..8 heap as `(float)u32`,
/// 12 the error flags. SOC2 ARM fields by record offset (serializer `0x8e4440`); fields 11,
/// 12, 15, 22 and 23 are never set.
pub(super) fn diagnostics(source: &Diagnostics) -> DiagnosticsMessage {
    let core = |block: &Option<DiagnosticsBlock>| {
        let w = block.map_or([0; 16], |block| block.words);
        Some(DSPCommsDiagnosticsMessage {
            status1: nonzero(w[0]),
            status2: nonzero(w[1]),
            status3: nonzero(w[2]),
            status4: nonzero(w[3]),
            process_cpu: nonzero(f32::from_bits(w[5])),
            internal_heap_usage: nonzero(w[6] as f32),
            external_heap_usage: nonzero(w[7] as f32),
            internal_pm_heap_usage: nonzero(w[8] as f32),
            process_cpu_max: nonzero(f32::from_bits(w[9])),
            ext_dma_usage: nonzero(f32::from_bits(w[10])),
            error_flags: nonzero(w[12]),
        })
    };
    let record = source.soc2.map_or([0; 128], |record| record.0);
    let word = |at: usize| {
        let bytes = record[at..at + 4].try_into().expect("four bytes");
        nonzero(u32::from_le_bytes(bytes))
    };
    let long = |at: usize| {
        let bytes = record[at..at + 8].try_into().expect("eight bytes");
        nonzero(u64::from_le_bytes(bytes))
    };
    let soc2 = SOC2ARMCommsDiagnosticsMessage {
        usb_connected: nonzero(u32::from(record[0])),
        in_read_idx: word(0x04),
        in_write_idx: word(0x08),
        out_read_idx: word(0x0c),
        out_write_idx: word(0x10),
        usb_audio_rx_frame: long(0x18),
        usb_audio_tx_frame: long(0x20),
        usb_audio_tx_frame_skipped: word(0x28),
        usb_audio_tx_frame_aborted: word(0x2c),
        usb_audio_out_rd_wr_distance_average: word(0x30),
        usb_audio_in_rd_wr_distance_average: word(0x34),
        usb_audio_out_gaps_detected: word(0x38),
        usb_audio_out_gap_resets: word(0x3c),
        usb_audio_plus_one_sample_count: word(0x40),
        usb_audio_minus_one_sample_count: word(0x44),
        usb_audio_out_buffer_resets: nonzero(u32::from(record[0x48])),
        usb_audio_streaming_ep_enabled: word(0x4c),
        usb_audio_streaming_ep_disabled: word(0x50),
        usb_midi_out_count: long(0x58),
        usb_midi_in_count: long(0x60),
        usb_hid_in_count: long(0x68),
        usb_hid_out_count: long(0x70),
        ..Default::default()
    };
    let [soc1_core1, soc1_core2, soc2_core1, soc2_core2] = &source.cores;
    DiagnosticsMessage {
        action: Some(MessageAction::Update.into()),
        soc1_core1_diagnostics: core(soc1_core1),
        soc1_core2_diagnostics: core(soc1_core2),
        soc2_core1_diagnostics: core(soc2_core1),
        soc2_core2_diagnostics: core(soc2_core2),
        soc2_arm_core_diagnostics: Some(soc2),
        soc1_to_soc2_dropped_count: Some(source.hid_tx_failures),
        ..Default::default()
    }
}

/// ModuleStats (35) READ `0x19c434`, as the S7 capture holds it (C):
/// - `stats`: the controller map `+0x16c`: the [`report_rows`] and the default rows of the
///   [`looked_up`] ids without one. Order (I): stock walks a hash map; Tetrad sends id order.
/// - `size_relations`: the FitCheckHelper map, see [`size_relations`].
/// - `valid_id`: the compiler's id map (`+0x184`, `0x582904`) in id order, all valid: every
///   model with a blob (its own or its clone base's; blob scan `0x881af0`) and the boot ids
///   [`VALID_IDS`] (618 ids on CorOS 4.1.0).
/// - `core_stats`: one per SHARC core (`id` = core), the core capacity: the template
///   `0x155e1f8` minus the kernel ([`budgets`]), `reuse_pmco` set.
///
/// Zero fields are proto3 defaults and stay unset.
pub(super) fn module_stats(state: &State) -> ModuleStatsMessage {
    let Some(specs) = &state.specs else {
        return ModuleStatsMessage {
            valid_id: VALID_IDS.into_iter().map(valid).collect(),
            ..Default::default()
        };
    };
    let base = state.module_stats.get_or_init(|| StatsBase::new(specs));
    let mut stats = base.rows.clone();
    for id in state.current.iter().flat_map(looked_up) {
        stats.entry(id).or_insert(DEFAULT_ROW);
    }
    ModuleStatsMessage {
        stats: stats.iter().map(|(&id, stats)| item(id, stats)).collect(),
        ..base.message.clone()
    }
}

/// The parts of `module_stats` that follow from the catalog alone: the `report_rows` and
/// the message without `stats`. Host, QC-scale catalog: building them took 0.2 ms of the
/// 0.3 ms READ (A3).
pub struct StatsBase {
    rows: BTreeMap<u32, ModuleStats>,
    message: ModuleStatsMessage,
}

impl StatsBase {
    pub fn new(specs: &Specs) -> Self {
        let rows = report_rows(specs);
        let mut valid_ids: Vec<u32> = (specs.models.values())
            .filter(|model| model.blob.is_some())
            .map(|model| model.id)
            .chain(VALID_IDS)
            .collect();
        valid_ids.sort_unstable();
        valid_ids.dedup();
        let core_stats = (budgets(specs).into_iter().zip(0..))
            .map(|(budget, core)| {
                let capacity = ModuleStats {
                    reuse_pmco: true,
                    ..budget
                };
                item(core, &capacity)
            })
            .collect();
        let message = ModuleStatsMessage {
            size_relations: size_relations(specs, &rows),
            valid_id: valid_ids.into_iter().map(valid).collect(),
            core_stats,
            ..Default::default()
        };
        Self { rows, message }
    }
}

fn item(id: u32, stats: &ModuleStats) -> ModuleStatsItem {
    ModuleStatsItem {
        id: nonzero(id),
        cpu_percent: nonzero(stats.cpu),
        dm_heap_bytes: nonzero(stats.dm_heap),
        pm_heap_bytes: nonzero(stats.pm_heap),
        sd_heap_bytes: nonzero(stats.sd_heap),
        dmda_bytes: nonzero(stats.dmda),
        pmda_bytes: nonzero(stats.pmda),
        pmco_bytes: nonzero(stats.pmco),
        edma_percent: nonzero(stats.edma),
        singletons: nonzero(i32::try_from(stats.singletons).unwrap_or(i32::MAX)),
        latency: nonzero(i32::try_from(stats.latency).unwrap_or(i32::MAX)),
        reuse_pmco: nonzero(stats.reuse_pmco),
        has_sidechain: nonzero(stats.has_sidechain),
    }
}

/// The stats map `+0x16c` as the loader `0x8826a4` leaves it at boot: one row per module
/// report row (a clone without one has none; it shares its base's blob and stats) and the
/// [`FIXED_ROWS`] with only their CPU.
fn report_rows(specs: &Specs) -> BTreeMap<u32, ModuleStats> {
    (specs.models.values())
        .filter_map(|model| {
            let stats = model.stats?;
            let row = if FIXED_ROWS.contains(&model.id) {
                ModuleStats {
                    cpu: stats.cpu,
                    ..DEFAULT_ROW
                }
            } else if model.report_row {
                stats
            } else {
                return None;
            };
            Some((model.id, row))
        })
        .collect()
}

/// The ids the compiler looks up in the stats map while it compiles the current preset
/// (`0x87fc2c`). A miss inserts [`DEFAULT_ROW`] (`operator[]` `0x883ed4`); the S7 capture
/// shows them as 12031 (a cab) and 27000 (the Looper X) of Factory 1A. Lookups:
/// - `0x880114`: every graph module except the mixer and the cab engines 12000, 12100, 12014,
///   12114 and 12116;
/// - `0x87ba40` (run-list entry `0x87b904`): the placed model; the FIR engines 12000 and
///   12100 place the block's own cab model.
///
/// Stock keeps the rows until reboot, so it also lists the modules of earlier presets; Tetrad
/// lists the current preset's (I). The mixer path (`0x88015c`) does no lookup (I).
fn looked_up(current: &Current) -> impl Iterator<Item = u32> + '_ {
    let deployment = &current.deployment;
    let groups = &deployment.plan().assignment.groups;
    (groups.iter().flat_map(|group| &group.items)).filter_map(|item| match item.stats_model.0 {
        ids::MIXER => None,
        engine @ (12_000 | 12_100) => {
            let block = item
                .node
                .site()
                .and_then(|site| deployment.layout().instance(site));
            Some(block.map_or(engine, |instance| instance.model.0))
        }
        id => Some(id),
    })
}

/// The FitCheckHelper size relations (helper `+0x78`, controller `+0x8a68`). `0x6ba7b8`
/// builds them once at boot from the [`report_rows`]; per category `0x6ba188` compares every
/// pair of models. A model takes part when (C):
/// - its category is in the ModelRepo visible map (`+0x30`, built by `0x5518d4`): not hidden
///   (`0x55069c`) and not in [`UNRELATED_CATEGORIES`];
/// - it is in that map: not hidden (`+0xa8`) and not replaced (`+0x10`, set by the
///   `replaces` list of another model);
/// - it has a blob (`+0x9c` clear: a `blob` attribute, or a clone base's) and a stats row.
///
/// When one row fits in the other (`0x5f8e60`, the fields of [`ModuleStats::fits`]), the
/// smaller lists the larger in `larger_models` and the larger lists the smaller in
/// `smaller_models`; equal rows list each other in both. Only models with a relation have a
/// row; the lists are in id order (`std::set`). Row order (I): stock walks a hash map;
/// Tetrad sends id order. This gives the 353 rows of the S7 capture exactly. Tetrad reads
/// an empty `blob` attribute as none (stock: a blob) and does not read a model `disabled`
/// attribute (stock: sets `+0x9c`); neither changes a row on CorOS 4.1.0 (I).
fn size_relations(
    specs: &Specs,
    rows: &BTreeMap<u32, ModuleStats>,
) -> Vec<ModuleStatsSizeRelation> {
    let replaced: BTreeSet<u32> = (specs.models.values())
        .flat_map(|model| model.replaces.iter().copied())
        .collect();
    let mut relations: BTreeMap<u32, ModuleStatsSizeRelationPair> = BTreeMap::new();
    let visible = (specs.categories.values())
        .filter(|category| !category.hidden && !UNRELATED_CATEGORIES.contains(&category.id));
    for category in visible {
        let models: Vec<(u32, &ModuleStats)> = (category.model_ids.iter())
            .filter_map(|id| specs.models.get(id))
            .filter(|model| !model.hidden && !replaced.contains(&model.id) && model.blob.is_some())
            .filter_map(|model| Some((model.id, rows.get(&model.id)?)))
            .collect();
        for (index, &(a, a_stats)) in models.iter().enumerate() {
            for &(b, b_stats) in &models[index + 1..] {
                if a_stats.fits(b_stats) {
                    relations.entry(a).or_default().larger_models.push(b);
                    relations.entry(b).or_default().smaller_models.push(a);
                }
                if b_stats.fits(a_stats) {
                    relations.entry(a).or_default().smaller_models.push(b);
                    relations.entry(b).or_default().larger_models.push(a);
                }
            }
        }
    }
    (relations.into_iter())
        .map(|(id, mut pair)| {
            pair.smaller_models.sort_unstable();
            pair.larger_models.sort_unstable();
            ModuleStatsSizeRelation {
                id: nonzero(id),
                relations: Some(pair),
            }
        })
        .collect()
}

fn valid(id: u32) -> ModuleStatsValidModelId {
    ModuleStatsValidModelId {
        id: Some(id),
        valid: Some(true),
    }
}

/// NC Recorder, NC Trainer and NC Refiner: the rows `0x8826a4` adds after the report.
const FIXED_ROWS: [u32; 3] = [20000, 20001, 20002];

/// A new stats row (ctor `0x5f9180`): all zero, `reuse_pmco` set.
const DEFAULT_ROW: ModuleStats = ModuleStats {
    cpu: 0.0,
    dm_heap: 0,
    pm_heap: 0,
    sd_heap: 0,
    dmda: 0,
    pmda: 0,
    pmco: 0,
    edma: 0.0,
    singletons: 0,
    latency: 0,
    reuse_pmco: true,
    has_sidechain: false,
};

/// Categories `0x6ba7b8` gives no size relations: the cabs (12, 21, 32, 33), the FX loops
/// (13) and the loopers (27).
const UNRELATED_CATEGORIES: [u32; 6] = [12, 13, 21, 27, 32, 33];

/// The compiler id map `+0x184` as `0x882148` leaves it at boot: these ids set valid. The
/// blob scan (`0x881af0`) adds every model whose blob it finds (I: every ModelRepo blob is
/// installed).
const VALID_IDS: [u32; 17] = [
    10003, 10004, 11000, 13000, 13001, 13002, 13003, 13004, 13005, 13006, 13007, 13008, 20000,
    20001, 20002, 22004, 27000,
];
