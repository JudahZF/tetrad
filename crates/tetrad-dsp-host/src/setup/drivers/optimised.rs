//! OptimisedAdvancedFlangerDriver (7014: constructor `0x56ff8c`, `+0x30`
//! `0x56fd68`) and OptimisedAdvancedChorusDriver (7015: `0x57103c`,
//! `0x570f84`).
//!
//! The constructors build a per-index range vector (0x1c-byte entries,
//! default 0..1 skew 1, no `std::function`); the filter sends a few indices
//! as display values through it (`0x64b8f8` mode 0) and passes the rest.
//! The flanger's SMOOTHING range follows WAVEFORM.

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode, StockPayload, StockTables, TableError};
use super::catalog::to_display;
use super::reload::{payload, words};
use super::{Driver, DriverKind};

/// SMOOTHING display minimum and maximum per WAVEFORM (`.rodata`
/// `0xf56950`, `0xf56970`: seven f32 each).
const SMOOTHING_RANGES: StockPayload = payload!(0x00f5_6950 => 0x3c);
const WAVES: usize = 7;
const WAVEFORM: u32 = 8;
const SMOOTHING: u32 = 11;

pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    std::iter::once(SMOOTHING_RANGES)
}

/// The indices each filter converts, with their constructor ranges.
fn range(chorus: bool, index: u32) -> Option<(f32, f32, f32)> {
    Some(match (chorus, index) {
        (_, 1) => (0.1, 10.0, 0.4),
        (false, 4) => (0.1, 10.0, 0.6),
        (false, 10) => (-1.0, 1.0, 1.0),
        (false, 12) | (true, 11) => (2000.0, 12000.0, 0.4),
        (false, 13) | (true, 12) => (20.0, 2000.0, 0.4),
        (false, 20) | (true, 16) => (-12.0, 12.0, 1.0),
        (true, 4) => (1.0, 50.0, 0.3),
        (true, 10) => (500.0, 8000.0, 0.4),
        _ => return None,
    })
}

/// Flanger state: `+0x24` WAVEFORM step, `+0x28` SMOOTHING.
#[derive(Debug)]
struct Smoothing {
    wave: usize,
    value: f32,
    /// Stock-derived ranges.
    ranges: [(f32, f32); WAVES],
}

impl Smoothing {
    fn display(&self) -> f32 {
        let (min, max) = self.ranges[self.wave];
        to_display(self.value, min, max, 1.0)
    }
}

#[derive(Debug)]
pub(super) struct Optimised {
    target: DeploymentTarget,
    /// `None` for the chorus.
    flanger: Option<Smoothing>,
}

impl Optimised {
    pub(super) fn flanger(
        identity: &ModelIdentity,
        tables: &impl StockTables,
    ) -> Result<Self, TableError> {
        let bytes = tables.read(SMOOTHING_RANGES)?;
        let word: Vec<f32> = words(&bytes).map(f32::from_bits).collect();
        if word.len() != 15 {
            return Err(TableError::Malformed);
        }
        Ok(Self {
            target: identity.target,
            flanger: Some(Smoothing {
                wave: 0,
                value: 0.0,
                ranges: std::array::from_fn(|wave| (word[wave], word[8 + wave])),
            }),
        })
    }

    pub(super) fn chorus(identity: &ModelIdentity) -> Self {
        Self {
            target: identity.target,
            flanger: None,
        }
    }
}

impl Driver for Optimised {
    fn kind(&self) -> DriverKind {
        match self.flanger {
            Some(_) => DriverKind::OptimisedAdvancedFlanger,
            None => DriverKind::OptimisedAdvancedChorus,
        }
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let target = self.target;
        if let Some((min, max, skew)) = range(self.flanger.is_none(), index) {
            out.push(DspWrite::parameter(
                target,
                index,
                to_display(value, min, max, skew),
            ));
            return false;
        }
        let Some(smoothing) = &mut self.flanger else {
            return true;
        };
        match index {
            WAVEFORM => {
                // `floorf(v·7)`, above 6 → 6, NaN → 0 (`vcvt.s32`). Stock
                // aborts below 0 (`array::at`); 0 here (owned).
                let step = (value * 7.0).floor();
                let wave = if step > 6.0 {
                    6
                } else {
                    (step as i32).max(0) as usize
                };
                if wave != smoothing.wave {
                    smoothing.wave = wave;
                    out.push(DspWrite::parameter(target, SMOOTHING, smoothing.display()));
                }
                true
            }
            SMOOTHING => {
                if value != smoothing.value {
                    smoothing.value = value;
                    out.push(DspWrite::parameter(target, SMOOTHING, smoothing.display()));
                }
                false
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::amps::tests::{Indexed, identity};
    use super::*;

    fn sent(driver: &mut Optimised, index: u32, value: f32) -> (bool, Vec<(u32, f32)>) {
        let mut out = Vec::new();
        let pass = driver.update_parameter(index, value, ParamSetMode::Push, &mut out);
        let writes = out
            .into_iter()
            .map(|write| match write {
                DspWrite::Parameter {
                    index, value_bits, ..
                } => (index, f32::from_bits(value_bits)),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        (pass, writes)
    }

    /// `0x56fd68`/`0x570f84`: converted indices go out as display values;
    /// masked ones pass; the flanger's SMOOTHING follows WAVEFORM's range
    /// (index table: wave `w` spans words `w` and `8 + w`, as `/128`).
    #[test]
    fn display_conversions_and_smoothing_range() {
        let mut chorus = Optimised::chorus(&identity(7015));
        assert_eq!(
            sent(&mut chorus, 4, 0.0),
            (false, vec![(4, 1.0)]),
            "DELAY 1..50 ms"
        );
        assert_eq!(sent(&mut chorus, 16, 0.5), (false, vec![(16, 0.0)]));
        assert_eq!(sent(&mut chorus, 13, 0.5), (true, vec![]));
        let mut flanger = Optimised::flanger(&identity(7014), &Indexed).unwrap();
        assert_eq!(sent(&mut flanger, 13, 1.0), (false, vec![(13, 2000.0)]));
        assert_eq!(sent(&mut flanger, 9, 0.5), (true, vec![]));
        assert_eq!(sent(&mut flanger, WAVEFORM, 0.0), (true, vec![]));
        assert_eq!(
            sent(&mut flanger, SMOOTHING, 0.5),
            (false, vec![(SMOOTHING, 0.5 * (8.0 / 128.0))])
        );
        assert_eq!(sent(&mut flanger, SMOOTHING, 0.5), (false, vec![]));
        assert_eq!(
            sent(&mut flanger, WAVEFORM, 1.0),
            (true, vec![(SMOOTHING, 10.0 / 128.0)])
        );
    }
}
