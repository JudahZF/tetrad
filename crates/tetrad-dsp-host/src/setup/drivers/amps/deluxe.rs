//! FenderDeluxeReverbDriver (1166 normal, 1167 vibrato): constructor
//! 0x561368, `+0x30` 0x5611e0, `+0x48` 0x56201c, timer slot 0x561e2c (same
//! body as `+0x48`), buffer helpers 0x561964, 0x561aac, 0x561bf8.

use std::sync::LazyLock;
use std::time::Duration;

use tetrad_core::conversion::{Breakpoint, Piecewise};
use tetrad_dsp_protocol::DeploymentTarget;

use super::{
    AmpTables, Driver, DriverKind, DspWrite, ModelIdentity, ParamSetMode, Stock, StockTables,
    TableError, Timer, metadata, send_buffers, word,
};

const CHANNEL: u32 = 1;
const VOLUME: u32 = 2;
/// Reload hold; stock never clears it after the upload.
const RELOAD: u32 = 6;
/// Int parameter 7 takes `0xaf0850[3·is_1167 + channel]`.
const WORD: u32 = 7;
/// `QTimer::start(10)` at 0x56128c.
const DELAY: Duration = Duration::from_millis(10);

/// 1166 normal-channel volume curve (constructor `+0x38..+0x5c`). Stock
/// inlines the `0x5cd110` law: a `vcmpe`/`bls` chain, then a fused
/// `y0 + (Δy/Δx)·(v − x0)`; NaN and values above 1 send 0.0.
static NORMAL_VOLUME: LazyLock<Piecewise> = LazyLock::new(|| {
    let points = [(0.0, 0.0), (0.2, 0.38), (0.5, 0.7), (0.8, 0.8), (1.0, 1.0)];
    Piecewise::new(points.map(|(x, y)| Breakpoint { x, y }).to_vec())
        .expect("five finite stock breakpoints")
});

/// Int parameter 7 per layout: 1166, then 1167.
pub(super) fn words(tables: &impl StockTables) -> Result<[Box<[u32]>; 2], TableError> {
    let read = |layouts: &[metadata::Channel]| {
        layouts
            .iter()
            .map(|layout| word(tables, layout.words[0]))
            .collect::<Result<Box<[u32]>, _>>()
    };
    Ok([
        read(&metadata::DELUXE_NORMAL)?,
        read(&metadata::DELUXE_VIBRATO)?,
    ])
}

#[derive(Debug)]
pub(crate) struct Deluxe {
    target: DeploymentTarget,
    /// Model 1166: the curve applies on channel 0.
    normal: bool,
    layouts: &'static [metadata::Channel],
    words: Stock<Box<[u32]>>,
    /// `+0x28`: `lroundf(v + v)`, starts at 1.
    channel: usize,
    /// `+0x2c`: the raw channel value of the last change, starts at −1.0.
    channel_value: f32,
    /// `+0x30`: the raw volume, starts at 2.0.
    volume: f32,
    dirty: bool,
    timer: Timer,
}

impl Deluxe {
    pub(crate) fn new(identity: &ModelIdentity, tables: &AmpTables) -> Self {
        let normal = identity.model_id == 1166;
        let (layouts, words): (&[metadata::Channel], _) = if normal {
            (&metadata::DELUXE_NORMAL, &tables.deluxe.0[0])
        } else {
            (&metadata::DELUXE_VIBRATO, &tables.deluxe.0[1])
        };
        Self {
            target: identity.target,
            normal,
            layouts,
            words: Stock(words.clone()),
            channel: 1,
            channel_value: -1.0,
            volume: 2.0,
            dirty: true,
            timer: Timer::default(),
        }
    }

    fn volume(&self) -> f32 {
        if self.channel == 0 {
            NORMAL_VOLUME.to_display(self.volume).unwrap_or(0.0)
        } else {
            self.volume
        }
    }

    fn flush(&mut self, out: &mut Vec<DspWrite>) {
        if self.timer.active || !self.dirty {
            return;
        }
        out.push(DspWrite::parameter(
            self.target,
            CHANNEL,
            self.channel_value,
        ));
        if self.normal {
            out.push(DspWrite::parameter(self.target, VOLUME, self.volume()));
        }
        out.push(DspWrite::IntParameter {
            target: self.target,
            index: WORD,
            value: self.words.0[self.channel],
        });
        send_buffers(self.target, &self.layouts[self.channel], 0..4, out);
        self.dirty = false;
    }
}

impl Driver for Deluxe {
    fn kind(&self) -> DriverKind {
        DriverKind::FenderDeluxeReverb
    }

    /// CHANNEL stores a change in every mode, but only edits outside the
    /// push mark dirty, hold the reload and start the idle timer: a pushed
    /// change after the first upload never reloads (stock). Rounded values
    /// beyond 0..=2 saturate (owned: stock indexes past its tables).
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            CHANNEL => {
                let channel = ((value + value).round() as usize).min(2);
                if channel != self.channel {
                    self.channel = channel;
                    self.channel_value = value;
                    if mode != ParamSetMode::Push && !self.timer.active {
                        self.dirty = true;
                        out.push(DspWrite::parameter(self.target, RELOAD, 1.0));
                        self.timer.start(DELAY);
                    }
                }
                false
            }
            VOLUME => {
                self.volume = value;
                if self.normal {
                    out.push(DspWrite::parameter(self.target, VOLUME, self.volume()));
                }
                !self.normal
            }
            RELOAD => false,
            _ => true,
        }
    }

    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        self.flush(out);
    }

    fn take_timer(&mut self) -> Option<Duration> {
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

    /// 1166 channel 0 sends the fused curve live and after push; NaN sends
    /// 0.0 (the `bls` chain never matches it). A recalled channel change
    /// holds the reload for 10 ms; the upload carries the stored raw value.
    #[test]
    fn deluxe_normal_curve_nan_and_ten_ms_reload() {
        let id = identity(1166);
        let target = id.target;
        let mut out = Vec::new();
        let mut driver = Deluxe::new(&id, &tables());
        assert!(!driver.update_parameter(CHANNEL, 0.0, ParamSetMode::Push, &mut out));
        assert!(!driver.update_parameter(VOLUME, 0.35, ParamSetMode::Push, &mut out));
        driver.update_parameter(VOLUME, f32::NAN, ParamSetMode::Live, &mut out);
        let curve = ((0.7_f32 - 0.38) / (0.5 - 0.2)).mul_add(0.35 - 0.2, 0.38);
        assert_eq!(
            out,
            [
                DspWrite::parameter(target, VOLUME, curve),
                DspWrite::parameter(target, VOLUME, 0.0)
            ]
        );
        out.clear();
        driver.parameters_pushed(&mut out);
        assert_eq!(out[0], DspWrite::parameter(target, CHANNEL, 0.0));
        assert_eq!(out[1], DspWrite::parameter(target, VOLUME, 0.0));
        assert_eq!(
            out[2],
            DspWrite::IntParameter {
                target,
                index: WORD,
                value: 2_f32.to_bits()
            }
        );
        assert_eq!(out.len(), 7);
        out.clear();
        driver.update_parameter(CHANNEL, 0.9, ParamSetMode::Recall, &mut out);
        assert_eq!(out, [DspWrite::parameter(target, RELOAD, 1.0)]);
        assert_eq!(driver.take_timer(), Some(DELAY));
        out.clear();
        driver.timer_elapsed(&mut out);
        assert_eq!(out[0], DspWrite::parameter(target, CHANNEL, 0.9));
        assert_eq!(
            out[3],
            DspWrite::StockData {
                target,
                buffer: 0,
                payload: metadata::DELUXE_NORMAL[2].buffers[0]
            }
        );
    }
}
