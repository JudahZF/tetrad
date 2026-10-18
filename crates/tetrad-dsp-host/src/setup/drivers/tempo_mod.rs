//! TempoModulationDriver: constructor VA `0x573c7c`, `+0x30` `0x574870`, rate writer `0x5747a0`,
//! tempo hook `+0x5c` `0x573a90`.
//!
//! Each model has one or two LFOs. RATE (free rate) and NOTE (tempo-synced
//! rate) never reach the DSP as is: the driver keeps both and writes the
//! active rate, in Hz through the inverse power law, to the RATE index.
//! SYNC selects the synced rate for both LFOs.

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::tables::{Segment, StockPayload, StockTables, TableError};
use super::super::{Block, DspWrite, ModelIdentity};
use super::catalog::{to_display, to_normalized};
use super::{Driver, DriverKind, ParamSetMode};

/// NOTE step → note length index (`.rodata` 0xf56ff8, 21 `i32`).
const NOTE_LENGTHS: StockPayload = StockPayload::new(&[Segment {
    va: 0x00f5_6ff8,
    len: NOTE_STEPS as u32 * 4,
}]);
/// NOTE step → dot index (`.rodata` 0xf57050, 21 `i32`).
const NOTE_DOTS: StockPayload = StockPayload::new(&[Segment {
    va: 0x00f5_7050,
    len: NOTE_STEPS as u32 * 4,
}]);
const NOTE_STEPS: usize = 21;

/// Both note tables, for [`super::stock_payloads`].
pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    [NOTE_LENGTHS, NOTE_DOTS].into_iter()
}

/// The two NOTE tables, read from the device's own ZenUI.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(test, derive(Default))]
pub struct NoteTables {
    lengths: [i32; NOTE_STEPS],
    dots: [i32; NOTE_STEPS],
}

impl NoteTables {
    pub fn read(tables: &impl StockTables) -> Result<Self, TableError> {
        let words = |payload| -> Result<[i32; NOTE_STEPS], TableError> {
            let bytes = tables.read(payload)?;
            let mut words = [0; NOTE_STEPS];
            for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(4)) {
                *word = i32::from_le_bytes(chunk.try_into().expect("4-byte chunk"));
            }
            Ok(words)
        };
        Ok(Self {
            lengths: words(NOTE_LENGTHS)?,
            dots: words(NOTE_DOTS)?,
        })
    }
}

/// Note lengths in beats (constructor `+0x30..`, `+0xb0..`).
const BEATS: [f32; 7] = [4.0, 2.0, 1.0, 0.5, 0.25, 0.125, 0.0625];
/// Plain, dotted, triplet (`+0x4c..`, `+0xcc..`; 0x3f2aaa3b is not 2/3).
const DOTS: [f32; 3] = [1.0, 1.5, f32::from_bits(0x3f2a_aa3b)];

/// One LFO's constructor constants.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LfoConfig {
    rate: u32,
    note: u32,
    /// NOTE display range (`+0x94`/`+0x98`), stepped by `floorf(x + 0.5)`.
    note_range: (u32, u32),
    /// RATE power law in Hz: min, max, skew (`+0x6c..+0x74`).
    law: (f32, f32, f32),
}

/// Per-model constants of the constructor switch (VA `0x573c7c`): LFO A,
/// the SYNC index, and LFO B for 7000 and 7008. No factory model sets a
/// rate `std::function` (7029's curve lambda `0x573930` has no factory path).
fn config(model_id: u32) -> Option<(LfoConfig, u32, Option<LfoConfig>)> {
    let lfo = |rate, note, low, law| LfoConfig {
        rate,
        note,
        note_range: (low, 20),
        law,
    };
    Some(match model_id {
        7000 => (
            lfo(2, 7, 10, (0.3, 3.0, 0.6)),
            6,
            Some(LfoConfig {
                note_range: (4, 12),
                ..lfo(3, 8, 4, (3.0, 13.0, 0.6))
            }),
        ),
        7001 => (lfo(2, 7, 7, (0.5, 6.5, 0.6)), 6, None),
        7002 => (lfo(0, 7, 4, (0.1, 15.0, 0.6)), 6, None),
        7003 => (lfo(1, 7, 4, (0.06, 13.0, 0.6)), 6, None),
        7004 => (lfo(0, 11, 4, (0.1, 20.0, 0.6)), 10, None),
        7006 => (lfo(1, 5, 4, (0.12, 10.0, 0.35)), 4, None),
        7007 => (lfo(1, 7, 7, (0.1, 7.6, 0.6)), 6, None),
        7008 => (
            lfo(1, 9, 4, (0.01, 9.9, 0.6)),
            8,
            Some(lfo(2, 10, 4, (0.1, 10.0, 0.6))),
        ),
        7009 => (lfo(1, 10, 4, (0.06, 13.0, 0.6)), 9, None),
        7031 => (lfo(1, 9, 4, (0.12, 10.0, 0.35)), 8, None),
        7032 => (lfo(0, 5, 4, (0.1, 10.0, 0.4)), 4, None),
        7033 => (lfo(1, 9, 4, (0.12, 10.0, 0.205)), 8, None),
        7035 => (lfo(1, 3, 10, (0.2, 3.0, 0.5)), 2, None),
        7036 => (lfo(0, 12, 4, (0.1, 20.0, 0.6)), 11, None),
        7037 => (lfo(1, 6, 7, (0.1, 5.0, 0.4)), 5, None),
        // Skew keeps the constructor default 1.0.
        7038 => (lfo(2, 7, 10, (0.3, 3.0, 1.0)), 6, None),
        _ => return None,
    })
}

/// Models whose SYNC and NOTE A values also go out unchanged (`0x574b0c`).
const PASS_SYNC_AND_NOTE: [u32; 2] = [7004, 7036];

#[derive(Debug, Clone, PartialEq)]
struct Lfo {
    config: LfoConfig,
    /// `+0x24` / `+0xa4`.
    sync: bool,
    /// `+0x68` / `+0xa0`: free rate in Hz. Stock leaves it unset until the
    /// RATE value arrives; 0.0 here (owned).
    free_rate: f32,
    /// `+0x58`, `+0x5c` (ms), `+0x60`, `+0x64`.
    dot: f32,
    period: f32,
    beats: f32,
    bpm: f32,
}

impl Lfo {
    fn new(config: LfoConfig) -> Self {
        Self {
            config,
            sync: false,
            free_rate: 0.0,
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

    /// Stock reads the dot table with an index below 7, past its 3 entries
    /// into the next fields.
    fn dot_slot(&self, index: u32) -> f32 {
        match index {
            0..=2 => DOTS[index as usize],
            3 => self.dot,
            4 => self.period,
            5 => self.beats,
            _ => self.bpm,
        }
    }

    /// NOTE value (`0x574870` tails): display step, then both tables.
    fn set_note(&mut self, value: f32, notes: &NoteTables) {
        let (low, high) = self.config.note_range;
        let step = (to_display(value, low as f32, high as f32, 1.0) + 0.5).floor() as i32;
        let (length, dot) = match usize::try_from(step).ok().filter(|&s| s < NOTE_STEPS) {
            Some(step) => (notes.lengths[step], notes.dots[step]),
            None => (0, 0),
        };
        if (length as u32) < 7 {
            self.beats = BEATS[length as usize];
        }
        self.update_period();
        if (dot as u32) < 7 {
            self.dot = self.dot_slot(dot as u32);
        }
        self.update_period();
    }

    /// Rate writer (`0x5747a0`): `1000/period` when synced, else the free
    /// rate, through the inverse power law, to the RATE index.
    fn write_rate(&self, target: DeploymentTarget, out: &mut Vec<DspWrite>) {
        let rate = if self.sync {
            1000.0 / self.period
        } else {
            self.free_rate
        };
        let (min, max, skew) = self.config.law;
        let value = to_normalized(rate, min, max, skew);
        out.push(DspWrite::parameter(target, self.config.rate, value));
    }
}

#[derive(Debug)]
pub(super) struct TempoModulation {
    target: DeploymentTarget,
    model_id: u32,
    sync: u32,
    a: Lfo,
    b: Option<Lfo>,
    notes: NoteTables,
}

impl TempoModulation {
    pub(super) fn new(identity: &ModelIdentity, notes: &NoteTables) -> Option<Self> {
        let (a, sync, b) = config(identity.model_id)?;
        Some(Self {
            target: identity.target,
            model_id: identity.model_id,
            sync,
            a: Lfo::new(a),
            b: b.map(Lfo::new),
            notes: notes.clone(),
        })
    }

    fn write_rates(&self, out: &mut Vec<DspWrite>) {
        self.a.write_rate(self.target, out);
        if let Some(b) = &self.b {
            b.write_rate(self.target, out);
        }
    }
}

impl Driver for TempoModulation {
    fn kind(&self) -> DriverKind {
        DriverKind::TempoModulation
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let pass = PASS_SYNC_AND_NOTE.contains(&self.model_id);
        let a = self.a.config;
        let b = self.b.as_ref().map(|b| b.config);
        let passes = if index == a.rate {
            let (min, max, skew) = a.law;
            self.a.free_rate = to_display(value, min, max, skew);
            false
        } else if index == self.sync {
            self.a.sync = value > 0.5;
            if let Some(b) = &mut self.b {
                b.sync = value > 0.5;
            }
            pass
        } else if index == a.note {
            self.a.set_note(value, &self.notes);
            pass
        } else if let (Some(config), Some(lfo)) = (b, &mut self.b)
            && (index == config.rate || index == config.note)
        {
            if index == config.rate {
                let (min, max, skew) = config.law;
                lfo.free_rate = to_display(value, min, max, skew);
            } else {
                lfo.set_note(value, &self.notes);
            }
            false
        } else {
            return true;
        };
        self.write_rates(out);
        passes
    }

    /// `0x573a90`: new BPM for both LFOs, then the rate writes.
    fn tempo_changed(&mut self, bpm: i32, out: &mut Vec<DspWrite>) {
        for lfo in std::iter::once(&mut self.a).chain(self.b.as_mut()) {
            lfo.bpm = bpm as f32;
            lfo.update_period();
        }
        self.write_rates(out);
    }
}

impl Block {
    /// Driver tempo hook (vtable `+0x5c`). Stock calls it for every driver
    /// from the tempo setter (VA `0x875a48`) before it sends `0x16`.
    pub fn tempo_changed(&mut self, bpm: i32, out: &mut Vec<DspWrite>) {
        if let Some(driver) = &mut self.driver {
            driver.tempo_changed(bpm, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use tetrad_dsp_protocol::{DspCore, ModuleSlot};

    use super::super::super::{BlockBypass, SetupValue};
    use super::*;
    use crate::setup::drivers::media::test_media::TestMedia;
    use crate::setup::{DriverTables, Factory};

    /// Synthetic tables: step s → length s % 7, dot s % 3.
    fn notes() -> NoteTables {
        NoteTables {
            lengths: std::array::from_fn(|s| (s % 7) as i32),
            dots: std::array::from_fn(|s| (s % 3) as i32),
        }
    }

    fn new_block(model_id: u32) -> (Block, DeploymentTarget) {
        let target = DeploymentTarget {
            core: DspCore::Core2,
            slot: ModuleSlot::try_from(4).unwrap(),
        };
        let identity = ModelIdentity {
            target,
            model_id,
            category: 7,
            fx_loop_send: None,
        };
        let mut out = Vec::new();
        let tables = DriverTables {
            notes: notes(),
            ..DriverTables::synthetic()
        };
        let factory = Factory {
            tables: Some(&tables),
            library: &TestMedia::default(),
            mini: false,
        };
        let block = Block::build(identity, &[], &factory, &mut out).unwrap();
        assert!(out.is_empty(), "the constructor sends nothing");
        (block, target)
    }

    fn period(dot: f32, beats: f32, bpm: f32) -> f32 {
        (f64::from(dot * beats) * 60.0 * 1000.0 / f64::from(bpm)) as f32
    }

    /// 7002 push: RATE goes out through the driver's power-law round trip,
    /// SYNC switches to the synced rate (default 500 ms period), NOTE
    /// re-times it and never passes (only 7004 and 7036 pass it); other
    /// indices pass unchanged.
    #[test]
    fn rate_sync_and_note_write_the_active_rate() {
        let (mut block, target) = new_block(7002);
        let mut values = vec![SetupValue::Float(0.25_f32.to_bits()); 10];
        values[6] = SetupValue::Float(1.0_f32.to_bits());
        // NOTE 0.4 → display 4 + 0.4·16 = 10.4 → step 10 → length 3, dot 1.
        values[7] = SetupValue::Float(0.4_f32.to_bits());
        let mut out = Vec::new();
        block
            .push(&values, BlockBypass::default(), None, &mut out)
            .unwrap();
        let law = |hz| to_normalized(hz, 0.1, 15.0, 0.6);
        let free = law(to_display(0.25, 0.1, 15.0, 0.6));
        let note = law(1000.0 / period(1.5, 0.5, 120.0));
        let parameters: Vec<(u32, f32)> = out
            .iter()
            .filter_map(|write| match *write {
                DspWrite::Parameter {
                    index, value_bits, ..
                } => Some((index, f32::from_bits(value_bits))),
                _ => None,
            })
            .collect();
        let mut expected = vec![(0, free)];
        expected.extend((1..6).map(|index| (index, 0.25)));
        expected.extend([(0, law(2.0)), (0, note)]);
        assert_eq!(parameters, expected);
        assert!(out.iter().all(|write| match *write {
            DspWrite::Parameter { target: t, .. } | DspWrite::Bypass { target: t, .. } =>
                t == target,
            _ => false,
        }));
    }

    /// Tempo hook: a new BPM re-times both LFOs of 7008 and rewrites both
    /// RATE indices. 7004 passes SYNC and NOTE through as well.
    #[test]
    fn tempo_change_rewrites_both_lfos() {
        let (mut block, target) = new_block(7008);
        let mut out = Vec::new();
        let values = vec![SetupValue::Float(1.0_f32.to_bits()); 13];
        block
            .push(&values, BlockBypass::default(), None, &mut out)
            .unwrap();
        out.clear();
        block.tempo_changed(60, &mut out);
        // NOTE 1.0 → step 20 → length 6 (0.0625 beats), dot 2 (triplet).
        let hz = 1000.0 / period(DOTS[2], 0.0625, 60.0);
        assert_eq!(
            out,
            [
                DspWrite::parameter(target, 1, to_normalized(hz, 0.01, 9.9, 0.6)),
                DspWrite::parameter(target, 2, to_normalized(hz, 0.1, 10.0, 0.6)),
            ]
        );
        let (mut block, target) = new_block(7004);
        let values = vec![SetupValue::Float(1.0_f32.to_bits()); 14];
        block
            .push(&values, BlockBypass::default(), None, &mut out)
            .unwrap();
        assert!(out.contains(&DspWrite::parameter(target, 10, 1.0)));
        assert!(out.contains(&DspWrite::parameter(target, 11, 1.0)));
    }

    /// The pinned ZenUI's NOTE tables hold in-range indices: lengths 0..7
    /// and dots 0..3, so the out-of-table branches never run in stock.
    #[test]
    #[ignore = "reads $QC_CORPUS_DIR/rootfs/opt/neuraldsp/ZenUI"]
    fn pinned_note_tables_hold_valid_indices() {
        use super::super::super::tables::ElfTables;
        let Some(path) = crate::corpus("rootfs/opt/neuraldsp/ZenUI") else {
            return;
        };
        let tables = ElfTables::drivers(std::fs::read(path).unwrap()).unwrap();
        let notes = NoteTables::read(&tables).unwrap();
        assert!(notes.lengths.iter().all(|length| (0..7).contains(length)));
        assert!(notes.dots.iter().all(|dot| (0..3).contains(dot)));
    }
}
