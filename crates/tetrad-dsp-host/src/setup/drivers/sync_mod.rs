//! Tempo-synced modulation drivers with ARM-side rate laws.
//!
//! MXRFlangerDriver (7021, 7042): constructor `0x572930`, `+0x30`
//! `0x572640`, rate `0x57229c`, sweep `0x572034`, tempo `+0x5c` `0x571bf8`.
//! SPEED (or the synced NOTE) becomes the hidden RATE, DELAY and DEPTH
//! through constructor curves.
//!
//! MishaThorDriver (7046): constructor `0x57375c`, `+0x30` `0x5733a8`,
//! tempo `0x5732b0`. AUTO SPEED and SYNC NOTE each drive one LFO: Hz to
//! indices 3 and 13.
//!
//! Both keep the TempoModulation LFO layout (`dot`, `period`, `beats`,
//! `bpm`) and read their own NOTE rows.

use std::sync::LazyLock;

use tetrad_core::conversion::Piecewise;
use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode, StockPayload, StockTables, TableError};
use super::catalog::to_display;
use super::dynamics::curve;
use super::reload::{payload, words};
use super::{Driver, DriverKind};

const STEPS: usize = 21;
/// NOTE step → length index, then dot index 0x58 later (`.rodata`, 21
/// `i32` each).
const MXR_NOTES: StockPayload = payload!(0x00f5_6b88 => 0xac);
const THOR_NOTES: StockPayload = payload!(0x00f5_6db8 => 0xac);
const DOT_ROW: usize = 0x58 / 4;

pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    [MXR_NOTES, THOR_NOTES].into_iter()
}

/// Note lengths in beats and plain/dotted/triplet factors (constructor
/// immediates; 0x3f2aaa3b is not 2/3).
const BEATS: [f32; 7] = [4.0, 2.0, 1.0, 0.5, 0.25, 0.125, 0.0625];
const DOTS: [f32; 3] = [1.0, 1.5, f32::from_bits(0x3f2a_aa3b)];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NoteRows {
    lengths: [i32; STEPS],
    dots: [i32; STEPS],
}

impl NoteRows {
    fn read(tables: &impl StockTables, payload: StockPayload) -> Result<Self, TableError> {
        let word: Vec<i32> = words(&tables.read(payload)?)
            .map(|word| word as i32)
            .collect();
        if word.len() != DOT_ROW + STEPS {
            return Err(TableError::Malformed);
        }
        Ok(Self {
            lengths: std::array::from_fn(|step| word[step]),
            dots: std::array::from_fn(|step| word[DOT_ROW + step]),
        })
    }
}

/// One LFO: `+0x74..+0x80` (MXR), `+0x58..+0x64` and `+0x9c..+0xa8`
/// (Thor).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lfo {
    dot: f32,
    period: f32,
    beats: f32,
    bpm: f32,
}

impl Lfo {
    const fn new() -> Self {
        Self {
            dot: 1.0,
            period: 500.0,
            beats: 1.0,
            bpm: 120.0,
        }
    }

    /// `f32(dot·beats)` widened, `·60·1000 / bpm` in f64.
    fn update_period(&mut self) {
        let beats = f64::from(self.dot * self.beats);
        self.period = (beats * 60.0 * 1000.0 / f64::from(self.bpm)) as f32;
    }

    /// A NOTE step (`vcvt.s32` of `floorf`): steps outside 0..=20 use row 0
    /// of neither table but length 0 and dot 0. A dot index past the three
    /// factors reads the next fields, as stock does.
    fn set_step(&mut self, step: i32, rows: &NoteRows) {
        let (length, dot) = match usize::try_from(step).ok().filter(|&s| s < STEPS) {
            Some(step) => (rows.lengths[step], rows.dots[step]),
            None => (0, 0),
        };
        if let Some(&beats) = usize::try_from(length).ok().and_then(|at| BEATS.get(at)) {
            self.beats = beats;
        }
        self.update_period();
        let slot = match dot {
            0..=2 => Some(DOTS[dot as usize]),
            3 => Some(self.dot),
            4 => Some(self.period),
            5 => Some(self.beats),
            6 => Some(self.bpm),
            _ => None,
        };
        if let Some(dot) = slot {
            self.dot = dot;
        }
        self.update_period();
    }

    fn rate(&self) -> f32 {
        1000.0 / self.period
    }
}

/// MXR curves (constructor `+0x84..+0x138`; inlined `0x5cd110` law).
fn bits(points: &[(u32, u32)]) -> Piecewise {
    let points: Vec<(f32, f32)> = points
        .iter()
        .map(|&(x, y)| (f32::from_bits(x), f32::from_bits(y)))
        .collect();
    curve(&points)
}
static REGEN: LazyLock<Piecewise> = LazyLock::new(|| {
    bits(&[
        (0, 0x3e2d_0e56),
        (0x3f00_0000, 0x3ec2_8f5c),
        (0x3f80_0000, 0x3f3c_ac08),
    ])
});
static SPEED_TO_RATE: LazyLock<Piecewise> = LazyLock::new(|| {
    bits(&[
        (0, 0x3da3_d70a),
        (0x3ea8_f5c3, 0x3e50_e560),
        (0x3f00_0000, 0x3ee8_f5c3),
        (0x3f2a_c083, 0x3f8e_147b),
        (0x3f40_0000, 0x4024_1893),
        (0x3f55_3f7d, 0x4080_0000),
        (0x3f80_0000, 0x40f8_a3d7),
    ])
});
static RATE_TO_SPEED: LazyLock<Piecewise> = LazyLock::new(|| {
    bits(&[
        (0x3da3_d70a, 0),
        (0x3e50_e560, 0x3ea8_f5c3),
        (0x3ee8_f5c3, 0x3f00_0000),
        (0x3f8e_147b, 0x3f2a_c083),
        (0x4024_1893, 0x3f40_0000),
        (0x4080_0000, 0x3f55_3f7d),
        (0x40f8_a3d7, 0x3f80_0000),
    ])
});
static DEPTH: LazyLock<Piecewise> = LazyLock::new(|| {
    bits(&[
        (0, 0xbf4d_3d86),
        (0x3ed9_999a, 0xbf41_a36f),
        (0x3f80_0000, 0xbde4_82bf),
    ])
});
static DELAY: LazyLock<Piecewise> = LazyLock::new(|| {
    bits(&[
        (0, 0x3f5e_afbd),
        (0x3f1c_28f6, 0x3f48_9a14),
        (0x3f80_0000, 0x3f06_bd0a),
    ])
});

/// `max(0.08, rate)` then `min(·, 7.77)` (std::max/min order: NaN → 0.08).
fn mxr_rate(rate: f32) -> f32 {
    let low = if rate > 0.08 { rate } else { 0.08 };
    if low >= 7.77 { 7.77 } else { low }
}

const MXR_RATE: u32 = 8;
const MXR_DELAY: u32 = 9;
const MXR_DEPTH: u32 = 10;

#[derive(Debug)]
pub(super) struct MxrFlanger {
    target: DeploymentTarget,
    /// 7042 Petrucci Flanger: OUTPUT − 1.9 dB.
    petrucci: bool,
    /// `+0x30`, `+0x34`, `+0x38`, `+0x40`.
    manual: f32,
    width: f32,
    speed: f32,
    sync: bool,
    lfo: Lfo,
    notes: NoteRows,
}

impl MxrFlanger {
    pub(super) fn new(
        identity: &ModelIdentity,
        tables: &impl StockTables,
    ) -> Result<Self, TableError> {
        Ok(Self::with_rows(
            identity,
            NoteRows::read(tables, MXR_NOTES)?,
        ))
    }

    fn with_rows(identity: &ModelIdentity, notes: NoteRows) -> Self {
        Self {
            target: identity.target,
            petrucci: identity.model_id == 7042,
            manual: 0.0,
            width: 0.0,
            speed: 0.0,
            sync: false,
            lfo: Lfo::new(),
            notes,
        }
    }

    /// `0x57229c`: RATE in Hz, then the sweep.
    fn send_rate(&self, out: &mut Vec<DspWrite>) {
        let rate = if self.sync {
            self.lfo.rate()
        } else {
            SPEED_TO_RATE.to_display(self.speed).unwrap_or(0.0)
        };
        out.push(DspWrite::parameter(self.target, MXR_RATE, mxr_rate(rate)));
        self.send_sweep(out);
    }

    /// `0x572034`: DELAY and DEPTH from SPEED, or from the synced rate
    /// mapped back to a SPEED position. Not fused.
    fn send_sweep(&self, out: &mut Vec<DspWrite>) {
        let position = if self.sync {
            RATE_TO_SPEED
                .to_display(mxr_rate(self.lfo.rate()))
                .unwrap_or(0.0)
        } else {
            self.speed
        };
        let delay = DELAY.to_display(position).unwrap_or(0.0);
        let depth = DEPTH.to_display(position).unwrap_or(0.0);
        let width = self.width;
        out.push(DspWrite::parameter(
            self.target,
            MXR_DELAY,
            self.manual * (1.0 - width) + width * delay,
        ));
        out.push(DspWrite::parameter(self.target, MXR_DEPTH, width * depth));
    }
}

impl Driver for MxrFlanger {
    fn kind(&self) -> DriverKind {
        DriverKind::MxrFlanger
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            1 => self.manual = value,
            2 => self.width = value,
            3 => self.speed = value,
            4 => self.sync = value > 0.5,
            5 => {
                let step = (to_display(value, 7.0, 20.0, 1.0) + 0.5).floor() as i32;
                self.lfo.set_step(step, &self.notes);
            }
            6 => {
                let regen = REGEN.to_display(value).unwrap_or(0.0) * 0.99;
                out.push(DspWrite::parameter(self.target, 6, regen));
                return false;
            }
            7 => {
                let mut db = to_display(value, -12.0, 12.0, 1.0);
                if self.petrucci {
                    db = (f64::from(db) - 1.9) as f32;
                }
                out.push(DspWrite::parameter(self.target, 7, db));
                return false;
            }
            MXR_RATE..=MXR_DEPTH => return false,
            _ => return true,
        }
        match index {
            1 | 2 => self.send_sweep(out),
            _ => self.send_rate(out),
        }
        // SYNC and NOTE also pass.
        matches!(index, 4 | 5)
    }

    fn tempo_changed(&mut self, bpm: i32, out: &mut Vec<DspWrite>) {
        self.lfo.bpm = bpm as f32;
        self.lfo.update_period();
        self.send_rate(out);
    }
}

const THOR_AUTO: u32 = 3;
const THOR_SYNC: u32 = 5;
const THOR_NOTE: u32 = 6;
const THOR_SYNC_RATE: u32 = 13;
const THOR_OUTPUT: u32 = 20;

/// SYNC RATE clamp: 0.1 unless above it (NaN → 0.1), at most 5.0.
fn thor_rate(rate: f32) -> f32 {
    if rate > 0.1 { rate.min(5.0) } else { 0.1 }
}

#[derive(Debug)]
pub(super) struct MishaThor {
    target: DeploymentTarget,
    /// AUTO SPEED (`+0x58..`) and SYNC NOTE (`+0x9c..`) LFOs.
    auto: Lfo,
    note: Lfo,
    notes: NoteRows,
}

impl MishaThor {
    pub(super) fn new(
        identity: &ModelIdentity,
        tables: &impl StockTables,
    ) -> Result<Self, TableError> {
        Ok(Self::with_rows(
            identity,
            NoteRows::read(tables, THOR_NOTES)?,
        ))
    }

    fn with_rows(identity: &ModelIdentity, notes: NoteRows) -> Self {
        Self {
            target: identity.target,
            auto: Lfo::new(),
            note: Lfo::new(),
            notes,
        }
    }
}

impl Driver for MishaThor {
    fn kind(&self) -> DriverKind {
        DriverKind::MishaThor
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let target = self.target;
        let mut send = |index, value| out.push(DspWrite::parameter(target, index, value));
        match index {
            // AUTO SPEED 0..5 → NOTE steps 19, 16, …, 4.
            THOR_AUTO => {
                let shown = to_display(value, 0.0, 5.0, 1.0);
                let step = (shown.mul_add(-3.0, 19.0) + 0.5).floor() as i32;
                self.auto.set_step(step, &self.notes);
                send(THOR_AUTO, self.auto.rate());
                false
            }
            THOR_SYNC => {
                if value > 0.5 {
                    send(THOR_SYNC_RATE, thor_rate(self.note.rate()));
                }
                true
            }
            THOR_NOTE => {
                let step = (to_display(value, 4.0, 20.0, 1.0) + 0.5).floor() as i32;
                self.note.set_step(step, &self.notes);
                send(THOR_SYNC_RATE, thor_rate(self.note.rate()));
                true
            }
            THOR_OUTPUT => {
                send(THOR_OUTPUT, to_display(value, -12.0, 12.0, 1.0));
                false
            }
            _ => true,
        }
    }

    fn tempo_changed(&mut self, bpm: i32, out: &mut Vec<DspWrite>) {
        for lfo in [&mut self.auto, &mut self.note] {
            lfo.bpm = bpm as f32;
            lfo.update_period();
        }
        out.push(DspWrite::parameter(
            self.target,
            THOR_AUTO,
            self.auto.rate(),
        ));
        out.push(DspWrite::parameter(
            self.target,
            THOR_SYNC_RATE,
            thor_rate(self.note.rate()),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::super::amps::tests::identity;
    use super::*;

    /// Synthetic rows: step s → length s % 7, dot s % 3.
    fn rows() -> NoteRows {
        NoteRows {
            lengths: std::array::from_fn(|s| (s % 7) as i32),
            dots: std::array::from_fn(|s| (s % 3) as i32),
        }
    }

    fn values(writes: &[DspWrite]) -> Vec<(u32, f32)> {
        writes
            .iter()
            .map(|write| match *write {
                DspWrite::Parameter {
                    index, value_bits, ..
                } => (index, f32::from_bits(value_bits)),
                ref other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    /// `0x572640`/`0x57229c`/`0x572034`: SPEED 0 sits on the first
    /// constructor points (0.08 Hz, DELAY 0x3f5eafbd, DEPTH 0xbf4d3d86);
    /// SYNC switches to the NOTE period and maps it back to a SPEED
    /// position; 7042 lowers OUTPUT by 1.9 dB in f64.
    #[test]
    fn mxr_rate_sweep_and_output_laws() {
        let mut mxr = MxrFlanger::with_rows(&identity(7042), rows());
        let mut out = Vec::new();
        assert!(!mxr.update_parameter(1, 0.5, ParamSetMode::Push, &mut out));
        assert!(!mxr.update_parameter(2, 0.5, ParamSetMode::Push, &mut out));
        out.clear();
        assert!(!mxr.update_parameter(3, 0.0, ParamSetMode::Push, &mut out));
        assert_eq!(
            values(&out),
            [
                (MXR_RATE, f32::from_bits(0x3da3_d70a)),
                (MXR_DELAY, 0.25 + 0.5 * f32::from_bits(0x3f5e_afbd)),
                (MXR_DEPTH, 0.5 * f32::from_bits(0xbf4d_3d86)),
            ]
        );
        out.clear();
        // NOTE step 13 → length 6 (1/16 beat), dot 1 (×1.5) at 120 BPM:
        // 46.875 ms, 21.33 Hz, clamped to 7.77.
        assert!(mxr.update_parameter(5, 6.0 / 13.0, ParamSetMode::Push, &mut out));
        assert!(mxr.update_parameter(4, 1.0, ParamSetMode::Push, &mut out));
        assert_eq!(values(&out[3..4]), [(MXR_RATE, 7.77)]);
        let position = RATE_TO_SPEED.to_display(7.77).unwrap();
        assert_eq!(
            values(&out[4..5]),
            [(MXR_DELAY, 0.25 + 0.5 * DELAY.to_display(position).unwrap())]
        );
        out.clear();
        mxr.update_parameter(7, 0.5, ParamSetMode::Push, &mut out);
        assert_eq!(values(&out), [(7, (0.0_f64 - 1.9) as f32)]);
        out.clear();
        mxr.update_parameter(6, 0.0, ParamSetMode::Push, &mut out);
        assert_eq!(values(&out), [(6, f32::from_bits(0x3e2d_0e56) * 0.99)]);
    }

    /// `0x5733a8`/`0x5732b0`: AUTO SPEED 1 (of 0..5) is step 16 → length
    /// 2 (1 beat), dot 1 (×1.5): 750 ms at 120 BPM; tempo rewrites both
    /// rates, SYNC RATE clamped to 0.1..5.
    #[test]
    fn thor_auto_speed_note_and_tempo() {
        let mut thor = MishaThor::with_rows(&identity(7046), rows());
        let mut out = Vec::new();
        assert!(!thor.update_parameter(THOR_AUTO, 0.2, ParamSetMode::Push, &mut out));
        assert_eq!(values(&out), [(THOR_AUTO, 1000.0 / 750.0)]);
        out.clear();
        assert!(thor.update_parameter(THOR_SYNC, 0.0, ParamSetMode::Push, &mut out));
        assert!(out.is_empty());
        assert!(thor.update_parameter(THOR_SYNC, 1.0, ParamSetMode::Push, &mut out));
        assert_eq!(values(&out), [(THOR_SYNC_RATE, 2.0)], "500 ms at start");
        out.clear();
        thor.tempo_changed(60, &mut out);
        assert_eq!(
            values(&out),
            [(THOR_AUTO, 1000.0 / 1500.0), (THOR_SYNC_RATE, 1.0)]
        );
        out.clear();
        assert!(!thor.update_parameter(THOR_OUTPUT, 1.0, ParamSetMode::Push, &mut out));
        assert_eq!(values(&out), [(THOR_OUTPUT, 12.0)]);
    }
}
