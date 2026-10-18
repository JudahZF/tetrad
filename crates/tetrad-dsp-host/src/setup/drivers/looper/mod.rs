//! LooperDriver (27000, 27002): constructor 0x55d6d4, vtable 0x12622e0
//! with `+0x30` 0x55e140, `+0x38` 0x55d190 (returns 0: a Looper block never
//! sends `0x0c`), `+0x48` 0x55d1a8 (empty), `+0x54` 0x55d1bc (MIDI CC);
//! routing 0x55d8f8, status timer slot 0x55e05c, status slot 0x55db24.
//!
//! Not a [`Block`](super::super::Block) driver: its writes are global
//! Looper requests (senders 0x875bd0 parameter, 0x875b64 event, 0x8758a4
//! insert path, 0x875c5c status) and router writes with blocking waits.
//! The constructor makes no DSP writes.

use std::time::Duration;

use tetrad_dsp_protocol::{
    DeploymentTarget,
    controls::ControlRequest,
    tuner_looper::{LooperStatus, TunerLooperRequest},
};

use super::super::{DspWrite, ParamSetMode, SetupError, StockPayload, StockTables, TableError};
use super::{DriverKind, catalog::to_display};

mod playback;
pub use playback::{
    LooperClock, LooperContext, LooperMidiClockStart, LooperPlayback, LooperProduct, TeardownMode,
};

/// One Looper write in stock order. `Wait` is a blocking `usleep`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LooperWrite {
    Request(TunerLooperRequest),
    Router(DspWrite),
    Wait(Duration),
    Control(ControlRequest),
    /// Rig write to this Looper block (0x55e140 → 0x523644), before DSP events.
    RigParameter {
        index: u32,
        value_bits: u32,
    },
}

/// The status timer repeats from the constructor (0x55d85c) until the
/// destructor (0x55d54c); see [`Looper::status_tick`].
pub const STATUS_PERIOD: Duration = Duration::from_millis(50);
/// `usleep(20000)` around global insert-path changes (0x55da70, 0x55db0c).
const PATH_WAIT: Duration = Duration::from_millis(20);
const PATHS: usize = 13;

macro_rules! segment {
    ($va:expr, $len:expr) => {
        StockPayload::new(&[super::super::tables::Segment { va: $va, len: $len }])
    };
}

/// Insert-path words for destinations 0 and 1 (`0xa55358`, `0xa55390`).
const INSERT_PATHS: [StockPayload; 2] = [
    segment!(0xa5_5358, PATHS as u32 * 4),
    segment!(0xa5_5390, PATHS as u32 * 4),
];
/// QUANTIZE values, u32 words sent as `vcvt.f32.u32` (0x55e55c).
const QUANTIZE: StockPayload = segment!(0xa5_53c8, 10 * 4);
/// PRE ROLL values, f32 (0x55e5d4).
const PRE_ROLL: StockPayload = segment!(0xa5_53f0, 4 * 4);

pub fn payloads() -> impl Iterator<Item = StockPayload> {
    INSERT_PATHS.into_iter().chain([QUANTIZE, PRE_ROLL])
}

/// The stock words the Looper reads, read once at start-up.
#[derive(Clone, Copy)]
pub struct LooperTables {
    insert_paths: [[u32; PATHS]; 2],
    quantize: [f32; 10],
    pre_roll: [f32; 4],
}

impl std::fmt::Debug for LooperTables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LooperTables(<stock data>)")
    }
}

fn words<const N: usize>(
    tables: &impl StockTables,
    payload: StockPayload,
) -> Result<[u32; N], TableError> {
    let bytes = tables.read(payload)?;
    if bytes.len() != N * 4 {
        return Err(TableError::Malformed);
    }
    Ok(std::array::from_fn(|i| {
        u32::from_le_bytes([
            bytes[4 * i],
            bytes[4 * i + 1],
            bytes[4 * i + 2],
            bytes[4 * i + 3],
        ])
    }))
}

impl LooperTables {
    pub fn read(tables: &impl StockTables) -> Result<Self, TableError> {
        Ok(Self {
            insert_paths: [
                words(tables, INSERT_PATHS[0])?,
                words(tables, INSERT_PATHS[1])?,
            ],
            quantize: words::<10>(tables, QUANTIZE)?.map(|word| word as f32),
            pre_roll: words::<4>(tables, PRE_ROLL)?.map(f32::from_bits),
        })
    }
}

/// The compiler's looper router placements (`+0x694` sends, `+0x6a4`
/// returns; accessors 0x5818a8, 0x5818c0).
#[derive(Debug, Clone, Copy, Default)]
pub struct Placements {
    pub sends: [Option<DeploymentTarget>; 2],
    pub returns: [Option<DeploymentTarget>; 2],
}

/// The factory's router choice (0x50f23c, 0x50fab4): `mode` takes int
/// parameter 0 (constructor `+0x2c`, `+0x34`), `gain` parameters 9 and 7
/// (`+0x3c`, `+0x44`). Stock sends to the first of each without a valid
/// test; a missing one is refused here (owned).
#[derive(Debug, Clone, Copy)]
struct Routing {
    mode: (DeploymentTarget, Option<DeploymentTarget>),
    gain: (DeploymentTarget, Option<DeploymentTarget>),
}

/// Per-block state for `+0x30`, routing and status reconcile. The caller
/// owns the longer-lived [`LooperPlayback`] controller state.
#[derive(Debug)]
pub struct Looper {
    model_id: u32,
    routing: Routing,
    tables: LooperTables,
    /// `+0xd0`: the last routed path, −1 (`None`) at first.
    path: Option<usize>,
    /// `+0x50`: the last QUANTIZE value sent.
    quantize: f32,
    /// `+0x58`, zeroed by 0x55d6d4; distinct from controller incoming status.
    last_status: LooperStatus,
    /// `+0xbc..+0xc0`, cleared by 0x55d6d4. On a mini (gate 0x499a60) the
    /// constructor then sets `+0xbd..+0xc0` (0x55d89c..0x55d8b8).
    duplicate_again: bool,
    record_latched: bool,
    overdub_latched: bool,
    punch_latched: bool,
    duplicate_sync: bool,
    /// `+0xc4` and `+0xc8`, set by Undo/Redo (0x55e140).
    undo_count: u32,
    operation_pending: bool,
    /// `+0xd5`: P20's selected index is nonzero (0x55e5d4).
    pre_roll: bool,
}

impl Looper {
    /// The QC constructor; see [`Self::for_product`].
    pub fn new(
        model_id: u32,
        placements: Placements,
        tables: &LooperTables,
    ) -> Result<Self, SetupError> {
        Self::for_product(model_id, placements, tables, LooperProduct::Qc)
    }

    /// Constructor 0x55d6d4. On a mini, record, overdub and punch are latched
    /// and duplicate sync is on for the life of the block.
    pub fn for_product(
        model_id: u32,
        placements: Placements,
        tables: &LooperTables,
        product: LooperProduct,
    ) -> Result<Self, SetupError> {
        let missing = || SetupError::MissingLooperPlacement { model_id };
        let Placements { sends, returns } = placements;
        let mode = (sends[0].ok_or_else(missing)?, None);
        let routing = match model_id {
            27000 => Routing {
                mode,
                gain: (returns[0].ok_or_else(missing)?, None),
            },
            27002 => Routing {
                mode: (mode.0, returns[0]),
                gain: (sends[1].ok_or_else(missing)?, returns[1]),
            },
            _ => {
                return Err(SetupError::UnsupportedDriver {
                    model_id,
                    driver: DriverKind::Looper,
                });
            }
        };
        let mini = product == LooperProduct::Mini;
        Ok(Self {
            model_id,
            routing,
            tables: *tables,
            path: None,
            quantize: 0.0,
            last_status: LooperStatus::from_words([0; 22]),
            duplicate_again: false,
            record_latched: mini,
            overdub_latched: mini,
            punch_latched: mini,
            duplicate_sync: mini,
            undo_count: 0,
            operation_pending: false,
            pre_roll: false,
        })
    }

    /// One [`STATUS_PERIOD`] tick (0x55e05c): a track-0 status request,
    /// unless `/opt/neuraldsp/nopoll` is a regular file (`polling_disabled`).
    pub fn status_tick(&self, polling_disabled: bool, out: &mut Vec<LooperWrite>) {
        if !polling_disabled {
            out.push(LooperWrite::Request(
                TunerLooperRequest::LooperStatusRequest { track: 0 },
            ));
        }
    }

    /// Compatible parameter path without controller state or caller gates;
    /// branches that read them return `UnrecoveredPath`.
    pub fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<LooperWrite>,
    ) -> Result<(), SetupError> {
        self.set_parameter(index, value, mode, None, &LooperContext::default(), out)
    }

    /// `+0x30` (0x55e140); `Live` is source 0. `playback` and `context` are
    /// required only on the branches that read them. Errors change nothing.
    pub fn set_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        playback: Option<&mut LooperPlayback>,
        context: &LooperContext,
        out: &mut Vec<LooperWrite>,
    ) -> Result<(), SetupError> {
        let live = mode == ParamSetMode::Live;
        // 0x55e15c: the only `vcmpe` (`movgt`); every release test is `!pressed`,
        // so NaN is a release.
        let pressed = value > 0.5;
        let (parameter, value) = match index {
            0..=7 if !live => return Ok(()),
            5 | 7 if pressed => (if index == 5 { 2 } else { 6 }, 1.0),
            5 | 7 => return Ok(()),
            0..=7 => {
                if self.operation_pending {
                    return Ok(()); // 0x55e140: Half Speed/Reverse skip +0xc8.
                }
                let playback = playback
                    .ok_or_else(|| self.unrecovered(0x55e1fc, "caller-owned LooperPlayback"))?;
                return self.button(index, pressed, playback, context, out);
            }
            // PLAYBACK LEVEL, OVERDUB LEVEL, THRESHOLD: `powf(10, dB·0.05)`
            // over −60..0 dB; exactly 0 (also −0) sends 0.
            8 | 9 | 12 => {
                let (parameter, skew) = match index {
                    8 => (0, 0x4073_50b1),
                    9 => (1, 0x4073_50b1),
                    _ => (3, 0x3fe6_a162),
                };
                let gain = if value == 0.0 {
                    0.0
                } else {
                    10_f32.powf(to_display(value, -60.0, 0.0, f32::from_bits(skew)) * 0.05)
                };
                (parameter, gain)
            }
            // HIGH PASS, LOW PASS in Hz.
            10 | 11 => (index - 6, to_display(value, 20.0, 20_000.0, 0.3)),
            13..=16 => {
                let allowed = context
                    .mode_changes_allowed
                    .ok_or_else(|| self.unrecovered(0x49a754, "P13–16 mode-change gate"))?;
                // 0x55e4b8: P15 then clears dupwait whenever the kept +0xc0 is off.
                let duplicate_sync = if allowed {
                    pressed
                } else {
                    self.duplicate_sync
                };
                if index == 15 && !duplicate_sync {
                    playback
                        .ok_or_else(|| self.unrecovered(0x513e24, "caller-owned dupwait"))?
                        .dupwait = false;
                }
                if allowed {
                    *match index {
                        13 => &mut self.record_latched,
                        14 => &mut self.overdub_latched,
                        15 => &mut self.duplicate_sync,
                        _ => &mut self.punch_latched,
                    } = pressed; // 0x55e490..0x55e4ec: +0xbd/+0xbe/+0xc0/+0xbf.
                }
                return Ok(());
            }
            21 | 23.. => return Ok(()),
            17 => {
                self.route(value, mode, out);
                return Ok(());
            }
            // QUANTIZE (0x55e530): live edits skip an unchanged value. A send also
            // sets the controller flag `+0xb882` (0x513df0) when a playback is given.
            18 => {
                let step = (value.mul_add(9.0, 0.5) as u32).min(9) as usize;
                let quantize = self.tables.quantize[step];
                if live && quantize == self.quantize {
                    return Ok(());
                }
                self.quantize = quantize;
                if let Some(playback) = playback {
                    playback.quantize_enabled = quantize > 0.0;
                }
                (7, quantize)
            }
            // MIDI CLOCK START on (0x55e93c): 1 only with clock in on and clock out off.
            19 if pressed => {
                let settings = context
                    .midi_clock_start
                    .ok_or_else(|| self.unrecovered(0x55e93c, "MIDI gates 0x32dba4/0x32db8c"))?;
                (9, f32::from(settings.enabled && !settings.inhibited))
            }
            19 => (9, 0.0),
            // PRE ROLL: `vcvt.s32(roundf(display(v, 0, 3)))`, NaN → 0.
            20 => {
                let step = to_display(value, 0.0, 3.0, 1.0).round() as usize;
                self.pre_roll = step != 0; // 0x55e5d4: +0xd5 follows index, not value.
                (10, self.tables.pre_roll[step.min(3)])
            }
            // REC. LENGTH: whole display steps 0..=32.
            22 => (11, to_display(value, 0.0, 32.0, 1.0).round() as i32 as f32),
        };
        out.push(LooperWrite::Request(TunerLooperRequest::LooperParameter {
            parameter,
            value_bits: value.to_bits(),
        }));
        Ok(())
    }

    fn unrecovered(&self, va: u32, missing: &'static str) -> SetupError {
        SetupError::UnrecoveredPath {
            model_id: self.model_id,
            driver: DriverKind::Looper,
            va,
            missing,
        }
    }

    /// ROUTING MODE (index 17, 0x55e4f4 then 0x55d8f8): path
    /// `min(u32(fma(v, 13, 0.5)), 13)`; 0 is the block's local path, 1..=13
    /// a global insert path. Waits are skipped in the push.
    fn route(&mut self, value: f32, mode: ParamSetMode, out: &mut Vec<LooperWrite>) {
        let path = (value.mul_add(13.0, 0.5) as u32).min(PATHS as u32) as usize;
        let waits = mode != ParamSetMode::Push;
        let Routing { mode: modes, gain } = self.routing;
        let gain_param =
            |target, index, value| LooperWrite::Router(DspWrite::parameter(target, index, value));
        let mode_param = |target, value| {
            LooperWrite::Router(DspWrite::IntParameter {
                target,
                index: 0,
                value,
            })
        };
        let insert = |destination, path| {
            LooperWrite::Request(TunerLooperRequest::GlobalLooperInsertPath { destination, path })
        };
        if path == 0 {
            if waits && self.path != Some(0) {
                out.extend([
                    insert(0, u32::MAX),
                    insert(1, u32::MAX),
                    LooperWrite::Wait(PATH_WAIT),
                ]);
            }
            out.extend(
                [gain.0]
                    .into_iter()
                    .chain(gain.1)
                    .map(|target| gain_param(target, 9, 1.0)),
            );
            out.extend(
                [modes.0]
                    .into_iter()
                    .chain(modes.1)
                    .map(|target| mode_param(target, 8)),
            );
        } else {
            out.push(gain_param(gain.0, 9, 0.0));
            // 0x55dad0: the second gain target's core with the first's slot.
            if let Some(second) = gain.1 {
                let target = DeploymentTarget {
                    slot: gain.0.slot,
                    ..second
                };
                out.push(gain_param(target, 9, 0.0));
            }
            out.extend(
                [modes.0]
                    .into_iter()
                    .chain(modes.1)
                    .map(|target| mode_param(target, 0)),
            );
            if waits && self.path == Some(0) {
                out.push(LooperWrite::Wait(PATH_WAIT));
            }
            for (destination, words) in (0..).zip(&self.tables.insert_paths) {
                out.push(insert(destination, words[path - 1]));
            }
        }
        self.path = Some(path);
        out.extend(
            [gain.0]
                .into_iter()
                .chain(gain.1)
                .map(|target| gain_param(target, 7, 1.0)),
        );
    }
}

#[cfg(test)]
mod tests;
