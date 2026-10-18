//! CoryWongAmpSnobDriver (1175): constructor 0x57de30, `+0x30` 0x57dd54,
//! `+0x48` 0x57e668, timer slot 0x57e570 (same body as `+0x48`), buffer
//! helpers 0x57e1d8, 0x57e29c, 0x57e364.

use std::sync::Arc;

use tetrad_dsp_protocol::DeploymentTarget;

use super::{
    AmpTables, Driver, DriverKind, DspWrite, ModelIdentity, ParamSetMode, RELOAD_DELAY, Stock,
    StockTables, TableError, Timer, metadata, send_buffers, word,
};

/// Reload hold; stock never clears it after the upload.
const RELOAD: u32 = 9;
/// Buffer 3 copies its prefix raw and scales the coefficients from byte
/// `0xffa818[i]·4` (0x200 in every layout).
const SCALED_FROM: usize = 0x200;

/// Buffer 3 per layout (0x57e364): every coefficient after [`SCALED_FROM`]
/// times `powf(10.0f, db · 0.05f)`, all f32, with `db` the f32 word
/// `0xffa858[layout]`.
pub(super) fn scaled_buffers(tables: &impl StockTables) -> Result<Box<[Arc<[u8]>]>, TableError> {
    metadata::CORY
        .iter()
        .map(|layout| {
            let db = f32::from_bits(word(tables, layout.words[0])?);
            let gain = 10_f32.powf(db * 0.05);
            let mut bytes = tables.read(layout.buffers[3])?;
            let suffix = bytes.get_mut(SCALED_FROM..).ok_or(TableError::Malformed)?;
            for coefficient in suffix.chunks_exact_mut(4) {
                let value = f32::from_le_bytes([
                    coefficient[0],
                    coefficient[1],
                    coefficient[2],
                    coefficient[3],
                ]);
                coefficient.copy_from_slice(&(gain * value).to_le_bytes());
            }
            Ok(bytes.into())
        })
        .collect()
}

#[derive(Debug)]
pub(crate) struct CoryWong {
    target: DeploymentTarget,
    buffers: Stock<Box<[Arc<[u8]>]>>,
    /// Indices 1 and 2 (`+0x28`, `+0x2c`).
    switches: [bool; 2],
    dirty: bool,
    timer: Timer,
}

impl CoryWong {
    /// Sends the default buffer 0 (`0x10653b0`); it is never sent again.
    pub(crate) fn new(
        identity: &ModelIdentity,
        tables: &AmpTables,
        out: &mut Vec<DspWrite>,
    ) -> Self {
        send_buffers(identity.target, &metadata::CORY[0], 0..1, out);
        Self {
            target: identity.target,
            buffers: tables.cory_wong.clone(),
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
        send_buffers(self.target, &metadata::CORY[layout], 1..3, out);
        out.push(DspWrite::Data {
            target: self.target,
            buffer: 3,
            payload: self.buffers.0[layout].clone(),
        });
        self.dirty = false;
    }
}

impl Driver for CoryWong {
    fn kind(&self) -> DriverKind {
        DriverKind::CoryWongAmpSnob
    }

    /// Switches take `lroundf(v)` and store every change, but only edits
    /// outside the push with an idle timer mark dirty, hold the reload and
    /// start the timer: a pushed change after the first upload never
    /// reloads (stock). Rounded values beyond 0..=1 saturate (owned).
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let switch = match index {
            1 | 2 => &mut self.switches[index as usize - 1],
            RELOAD => return false,
            _ => return true,
        };
        let on = value.round() >= 1.0;
        if *switch != on {
            *switch = on;
            if mode != ParamSetMode::Push && !self.timer.active {
                self.dirty = true;
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

    /// Buffer 3 keeps its prefix and scales only the coefficient suffix by
    /// `10^(db·0.05)` (synthetic db word 2.0, coefficients 1.5).
    #[test]
    fn cory_scales_only_the_coefficient_suffix() {
        let id = identity(1175);
        let mut out = Vec::new();
        let mut driver = CoryWong::new(&id, &tables(), &mut out);
        assert_eq!(out.len(), 1);
        out.clear();
        driver.parameters_pushed(&mut out);
        let [
            ..,
            DspWrite::Data {
                buffer: 3, payload, ..
            },
        ] = &out[..]
        else {
            panic!("expected scaled buffer 3, got {out:?}");
        };
        let at =
            |offset: usize| f32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap());
        assert_eq!(payload.len(), metadata::CORY[0].buffers[3].len());
        assert_eq!(at(SCALED_FROM - 4), 1.5);
        assert_eq!(at(SCALED_FROM), 10_f32.powf(2.0 * 0.05) * 1.5);
        assert_eq!(out.len(), 3);
    }
}
