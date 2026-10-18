//! FortinNamelessDriver (1165): constructor 0x57d434, `+0x30` 0x57d37c,
//! `+0x48` 0x57db14, timer slot 0x57dc34 (same body as `+0x48`).

use tetrad_dsp_protocol::DeploymentTarget;

use super::{
    AmpTables, Driver, DriverKind, DspWrite, ModelIdentity, ParamSetMode, RELOAD_DELAY, Stock,
    StockTables, TableError, Timer, metadata, send_buffers, word,
};

/// Reload hold: 1.0 in the constructor and before a timed reload, 0.0 after.
const RELOAD: u32 = 10;
/// Int parameter 11 takes the layout's stock word (`0xf8f950[s0 + 2·s1]`).
const WORD: u32 = 11;

/// Int parameter 11 per layout. The word table is transposed relative to
/// the buffer tables; `metadata::FORTIN` already pairs them.
pub(super) fn words(tables: &impl StockTables) -> Result<Box<[u32]>, TableError> {
    metadata::FORTIN
        .iter()
        .map(|layout| word(tables, layout.words[0]))
        .collect()
}

#[derive(Debug)]
pub(crate) struct Fortin {
    target: DeploymentTarget,
    words: Stock<Box<[u32]>>,
    /// Indices 0 and 1 (`+0x28`, `+0x2c`).
    switches: [bool; 2],
    dirty: bool,
    timer: Timer,
}

impl Fortin {
    pub(crate) fn new(
        identity: &ModelIdentity,
        tables: &AmpTables,
        out: &mut Vec<DspWrite>,
    ) -> Self {
        out.push(DspWrite::parameter(identity.target, RELOAD, 1.0));
        Self {
            target: identity.target,
            words: tables.fortin.clone(),
            switches: [false; 2],
            dirty: true,
            timer: Timer::default(),
        }
    }

    fn flush(&mut self, out: &mut Vec<DspWrite>) {
        if self.timer.active || !self.dirty {
            return;
        }
        let layout = usize::from(self.switches[0]) * 2 + usize::from(self.switches[1]);
        for (index, on) in (0..).zip(self.switches) {
            out.push(DspWrite::parameter(
                self.target,
                index,
                f32::from(u8::from(on)),
            ));
        }
        out.push(DspWrite::IntParameter {
            target: self.target,
            index: WORD,
            value: self.words.0[layout],
        });
        send_buffers(self.target, &metadata::FORTIN[layout], 0..4, out);
        out.push(DspWrite::parameter(self.target, RELOAD, 0.0));
        self.dirty = false;
    }
}

impl Driver for Fortin {
    fn kind(&self) -> DriverKind {
        DriverKind::FortinNameless
    }

    /// Switches take `lroundf(v)`; a change marks dirty in every mode, and
    /// edits outside the push also hold the reload and start the timer
    /// when it is idle. Rounded values beyond 0..=1 saturate (owned: stock
    /// indexes past its tables).
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let Some(switch) = self.switches.get_mut(index as usize) else {
            return index != RELOAD;
        };
        let on = value.round() >= 1.0;
        if *switch != on {
            *switch = on;
            self.dirty = true;
            if mode != ParamSetMode::Push && !self.timer.active {
                out.push(DspWrite::parameter(self.target, RELOAD, 1.0));
                self.timer.start(RELOAD_DELAY);
            }
        }
        false
    }

    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        self.flush(out);
    }

    fn take_timer(&mut self) -> Option<std::time::Duration> {
        self.timer.pending.take()
    }

    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        self.timer.elapsed();
        self.flush(out);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{identity, tables};
    use super::*;

    /// Layout (s0, s1) = (1, 0) uses buffer row 2 but word `0xf8f950[1]`;
    /// a recalled change holds the reload once and waits for the timer.
    #[test]
    fn fortin_pairs_transposed_word_with_buffers_and_waits_for_reload() {
        let id = identity(1165);
        let mut out = Vec::new();
        let mut driver = Fortin::new(&id, &tables(), &mut out);
        assert_eq!(out, [DspWrite::parameter(id.target, RELOAD, 1.0)]);
        assert_eq!(
            metadata::FORTIN[2].words[0],
            super::super::payload!(0xf8f954 => 4)
        );
        out.clear();
        assert!(!driver.update_parameter(0, 1.0, ParamSetMode::Push, &mut out));
        assert!(driver.update_parameter(2, 0.3, ParamSetMode::Push, &mut out));
        assert!(!driver.update_parameter(RELOAD, 0.3, ParamSetMode::Push, &mut out));
        driver.parameters_pushed(&mut out);
        assert_eq!(
            out[2],
            DspWrite::IntParameter {
                target: id.target,
                index: WORD,
                value: 2_f32.to_bits()
            }
        );
        assert_eq!(
            out[3],
            DspWrite::StockData {
                target: id.target,
                buffer: 0,
                payload: metadata::FORTIN[2].buffers[0]
            }
        );
        assert_eq!(out.len(), 8);
        out.clear();
        driver.update_parameter(1, 1.0, ParamSetMode::Recall, &mut out);
        driver.update_parameter(0, 0.0, ParamSetMode::Recall, &mut out);
        assert_eq!(out, [DspWrite::parameter(id.target, RELOAD, 1.0)]);
        assert_eq!(driver.take_timer(), Some(RELOAD_DELAY));
        out.clear();
        driver.scene_recalled(&|_| None, &mut out);
        driver.parameters_pushed(&mut out);
        assert!(out.is_empty());
        driver.timer_elapsed(&mut out);
        assert_eq!(
            out[3],
            DspWrite::StockData {
                target: id.target,
                buffer: 0,
                payload: metadata::FORTIN[1].buffers[0]
            }
        );
        assert_eq!(
            out.last(),
            Some(&DspWrite::parameter(id.target, RELOAD, 0.0))
        );
    }
}
