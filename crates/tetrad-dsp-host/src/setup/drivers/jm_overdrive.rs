//! JMOverdriveDriver (46, Tealbreaker): constructor `0x58068c`, `+0x30`
//! `0x580398`, `+0x48` `0x580e40` = timer slot `0x580c00`, buffer 3
//! `0x580b24`.
//!
//! MODEL picks the TS or BB circuit. In TS the controls pass raw (as the
//! driver's own writes; `+0x30` always returns false). In BB they go through
//! ARM laws, and GAIN also drives the output gain at the header row's first
//! word. A MODEL change outside the push mutes that output and reloads 20 ms
//! later: header row (word 11 forced to 1), two stock buffers, a generated
//! buffer 3, then a replay of GAIN, TONE and LEVEL.

use std::sync::{Arc, LazyLock};

use tetrad_core::conversion::Piecewise;
use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode, StockPayload, StockTables, TableError};
use super::catalog::{to_display, to_normalized};
use super::dynamics::curve;
use super::reload::{Timer, payload, words};
use super::{Driver, DriverKind};

const GAIN: u32 = 0;
const TONE: u32 = 1;
const LEVEL: u32 = 2;
const MODEL: u32 = 3;
/// `QTimer::start(20)` at 0x5804bc.
const RELOAD_MS: u64 = 20;
const HEADER_BYTES: u32 = 0x3c;
/// The header word the upload forces to 1 (`param_1[0x15]`).
const HEADER_FLAG: usize = 11;
const BUFFER3_TABLE: u32 = 0x158;

/// Per circuit (TS, BB): header row, buffers 1 and 2, buffer 3 source.
const HEADERS: [StockPayload; 2] = [
    payload!(0x011c_c5b4 => HEADER_BYTES),
    payload!(0x011c_c578 => HEADER_BYTES),
];
const BUFFERS: [[StockPayload; 2]; 2] = [
    [
        payload!(0x011d_3434 => 0x6e44),
        payload!(0x011d_c578 => 0x2300),
    ],
    [
        payload!(0x011c_c5f0 => 0x6e44),
        payload!(0x011d_a278 => 0x2300),
    ],
];
const BUFFER3: [StockPayload; 2] = [
    payload!(0x011d_e9d0 => BUFFER3_TABLE),
    payload!(0x011d_e878 => BUFFER3_TABLE),
];
/// Buffer 3 prefix: five f32 coefficients from the constructor (`+0xd4`
/// TS, `+0xe8` BB).
const COEFFICIENTS: [[u32; 5]; 2] = [
    [0x3f7f_d522, 0, 0xbf7f_d522, 0, 0xbf7f_aa45],
    [0xbf7f_d522, 0x8000_0000, 0x3f7f_d522, 0, 0xbf7f_aa45],
];

pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    HEADERS
        .into_iter()
        .chain(BUFFERS.into_iter().flatten())
        .chain(BUFFER3)
}

/// BB GAIN → output dB (constructor `+0xb0..+0xcc`) and BB LEVEL
/// (`+0x78..+0xac`); inlined `0x5cd110` law, NaN and values above 1 give 0.
static BB_OUTPUT: LazyLock<Piecewise> =
    LazyLock::new(|| curve(&[(0.0, 8.0), (0.25, 3.0), (0.5, 0.0), (1.0, 0.0)]));
static BB_LEVEL: LazyLock<Piecewise> = LazyLock::new(|| {
    curve(&[
        (0.0, 0.0),
        (0.1, 0.25),
        (0.25, 0.39),
        (0.5, 0.67),
        (0.66, 0.7),
        (0.9, 0.75),
        (1.0, 0.8),
    ])
});

/// One circuit's generated buffers and output index, from stock bytes.
#[derive(Clone)]
struct Circuit {
    header: Arc<[u8]>,
    buffer3: Arc<[u8]>,
    output: u32,
}

/// Stock-derived bytes; `Debug` never prints them.
impl std::fmt::Debug for Circuit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Circuit")
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

impl Circuit {
    fn read(tables: &impl StockTables, bb: usize) -> Result<Self, TableError> {
        let read = |payload: StockPayload| {
            let bytes = tables.read(payload)?;
            if bytes.len() == payload.len() {
                Ok(bytes)
            } else {
                Err(TableError::Malformed)
            }
        };
        let mut header = read(HEADERS[bb])?;
        let output = words(&header).next().ok_or(TableError::Malformed)?;
        header[HEADER_FLAG * 4..][..4].copy_from_slice(&1_u32.to_le_bytes());
        let table = read(BUFFER3[bb])?;
        let buffer3: Vec<u8> = COEFFICIENTS[bb]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .chain(table[0x100..].iter().copied())
            .chain(table[..0x100].iter().copied())
            .collect();
        Ok(Self {
            header: header.into(),
            buffer3: buffer3.into(),
            output,
        })
    }
}

#[derive(Debug)]
pub(super) struct JmOverdrive {
    target: DeploymentTarget,
    circuits: [Circuit; 2],
    /// `+0x24`: BB.
    bb: bool,
    /// `+0x28`: the uploaded header's first word. Stock leaves it unset
    /// before the first upload, which only push-mode values precede; the TS
    /// row's word here (owned).
    output: u32,
    /// `+0x64..`: the last value per index.
    values: [f32; 4],
    /// `+0x74`, set at construction.
    dirty: bool,
    timer: Timer,
}

impl JmOverdrive {
    pub(super) fn new(
        identity: &ModelIdentity,
        tables: &impl StockTables,
    ) -> Result<Self, TableError> {
        let circuits = [Circuit::read(tables, 0)?, Circuit::read(tables, 1)?];
        Ok(Self {
            target: identity.target,
            output: circuits[0].output,
            circuits,
            bb: false,
            values: [0.0; 4],
            dirty: true,
            timer: Timer::default(),
        })
    }

    fn reload(&mut self, out: &mut Vec<DspWrite>) {
        if self.timer.active || !self.dirty {
            return;
        }
        let bb = usize::from(self.bb);
        let circuit = &self.circuits[bb];
        self.output = circuit.output;
        let target = self.target;
        let data = |buffer, payload: &Arc<[u8]>| DspWrite::Data {
            target,
            buffer,
            payload: payload.clone(),
        };
        out.push(data(0, &circuit.header));
        out.extend(
            (1..)
                .zip(BUFFERS[bb])
                .map(|(buffer, payload)| DspWrite::StockData {
                    target,
                    buffer,
                    payload,
                }),
        );
        out.push(data(3, &circuit.buffer3));
        for index in [GAIN, TONE, LEVEL] {
            self.update_parameter(index, self.values[index as usize], ParamSetMode::Live, out);
        }
        self.dirty = false;
    }
}

impl Driver for JmOverdrive {
    fn kind(&self) -> DriverKind {
        DriverKind::JmOverdrive
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        if let Some(slot) = self.values.get_mut(index as usize) {
            *slot = value;
        }
        let target = self.target;
        let mut send = |index, value| out.push(DspWrite::parameter(target, index, value));
        match index {
            GAIN | TONE | LEVEL if !self.bb => send(index, value),
            GAIN => {
                send(GAIN, to_display(value, 0.35, 1.0, 2.5));
                let db = BB_OUTPUT.to_display(value).unwrap_or(0.0);
                send(self.output, to_normalized(db, -60.0, 12.0, 3.801_8));
            }
            TONE => send(TONE, to_display(value, 0.0, 1.0, 1.444)),
            LEVEL => send(LEVEL, BB_LEVEL.to_display(value).unwrap_or(0.0)),
            MODEL => {
                let bb = value != 0.0;
                if bb != self.bb {
                    self.bb = bb;
                    self.dirty = true;
                    if mode != ParamSetMode::Push && !self.timer.active {
                        send(self.output, 0.0);
                        self.timer.start(RELOAD_MS);
                    }
                }
            }
            _ => {}
        }
        false
    }

    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        self.reload(out);
    }

    fn take_timer(&mut self) -> Option<std::time::Duration> {
        self.timer.pending.take()
    }

    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        self.timer.elapsed();
        self.reload(out);
    }
}

#[cfg(test)]
mod tests {
    use super::super::amps::tests::{Synthetic, identity};
    use super::*;

    fn value(write: &DspWrite) -> (u32, f32) {
        match *write {
            DspWrite::Parameter {
                index, value_bits, ..
            } => (index, f32::from_bits(value_bits)),
            ref other => panic!("unexpected {other:?}"),
        }
    }

    /// `0x580398`/`0x580e40` with synthetic rows (every word 1.5f32): the
    /// push reloads once and replays TS values raw; a live MODEL change mutes
    /// the header's output index and the reload replays through the BB laws.
    #[test]
    fn model_switch_reloads_and_replays_through_bb_laws() {
        let mut jm = JmOverdrive::new(&identity(46), &Synthetic).unwrap();
        let output = 1.5_f32.to_bits();
        let mut out = Vec::new();
        for (index, value) in [(GAIN, 0.75), (TONE, 0.5), (LEVEL, 0.3), (MODEL, 0.0)] {
            assert!(!jm.update_parameter(index, value, ParamSetMode::Push, &mut out));
        }
        jm.parameters_pushed(&mut out);
        let DspWrite::Data {
            buffer: 0,
            ref payload,
            ..
        } = out[3]
        else {
            panic!("expected the header, got {:?}", out[3]);
        };
        assert_eq!(payload.len(), 0x3c);
        assert_eq!(payload[0x2c..0x30], 1_u32.to_le_bytes());
        let DspWrite::Data {
            buffer: 3,
            ref payload,
            ..
        } = out[6]
        else {
            panic!("expected buffer 3, got {:?}", out[6]);
        };
        assert_eq!(payload.len(), 0x16c);
        assert_eq!(payload[..4], 0x3f7f_d522_u32.to_le_bytes());
        assert_eq!(value(&out[7]), (GAIN, 0.75), "TS replays raw");
        out.clear();
        jm.update_parameter(MODEL, 1.0, ParamSetMode::Live, &mut out);
        assert_eq!(value(&out[0]), (output, 0.0));
        out.clear();
        jm.parameters_pushed(&mut out);
        assert!(out.is_empty(), "the timer holds the reload");
        jm.take_timer();
        jm.timer_elapsed(&mut out);
        let replay: Vec<_> = out[4..].iter().map(value).collect();
        // GAIN 0.75: (0.5, 0) → (1, 0) gives 0 dB.
        assert_eq!(
            replay,
            [
                (GAIN, to_display(0.75, 0.35, 1.0, 2.5)),
                (output, to_normalized(0.0, -60.0, 12.0, 3.801_8)),
                (TONE, to_display(0.5, 0.0, 1.0, 1.444)),
                // LEVEL 0.3 on (0.25, 0.39) → (0.5, 0.67), fused.
                (
                    LEVEL,
                    ((0.67_f32 - 0.39) / (0.5 - 0.25)).mul_add(0.3 - 0.25, 0.39)
                ),
            ]
        );
    }
}
