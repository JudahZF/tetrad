//! Compressor drivers.
//!
//! SSLBusCompDriver (5017, 5022, 5023, 5030): constructor `0x5697e0`,
//! `+0x30` `0x56a01c`, law `0x569990`. COMP, LEVEL and the ATTACK switch
//! never reach the DSP; each of them recomputes the hidden THRESHOLD,
//! RATIO, ATTACK, RELEASE, MAKEUP and INPUT GAIN values on the ARM.
//!
//! HorizonClarityCompDriver (5029): constructor `0x5622ac`, `+0x30`
//! `0x56220c`, `+0x48` `0x56252c` = timer slot `0x5624f0`, upload
//! `0x5623e4`. INTENSITY selects one of two table sets.

use std::sync::LazyLock;

use tetrad_core::conversion::{Breakpoint, Piecewise};
use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode, StockPayload};
use super::catalog::to_normalized;
use super::reload::{Timer, payload};
use super::{Driver, DriverKind};

const COMP: u32 = 9;
const LEVEL: u32 = 10;
const ATTACK: u32 = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ssl {
    /// 5017 Plini Comp.
    Plini,
    /// 5022 SLO-100 Compressor.
    Slo,
    /// 5023 Nolly Compressor.
    Nolly,
    /// 5030 Tim Henson Compressor.
    Henson,
}

/// Tim Henson MAKEUP offset in dB over COMP (constructor `+0x30..+0x54` for
/// a fast ATTACK, `+0x58..+0x7c` otherwise). Inlined `0x5cd110` law.
static HENSON_FAST: LazyLock<Piecewise> = LazyLock::new(|| {
    curve(&[
        (0.0, 0.0),
        (0.25, -2.0),
        (0.5, 0.0),
        (0.75, 7.0),
        (1.0, 14.0),
    ])
});
static HENSON_SLOW: LazyLock<Piecewise> = LazyLock::new(|| {
    curve(&[
        (0.0, 0.0),
        (0.25, -2.0),
        (0.5, -1.0),
        (0.75, 4.0),
        (1.0, 9.0),
    ])
});

pub(super) fn curve(points: &[(f32, f32)]) -> Piecewise {
    Piecewise::new(points.iter().map(|&(x, y)| Breakpoint { x, y }).collect())
        .expect("finite stock breakpoints")
}

#[derive(Debug)]
pub(super) struct SslBusComp {
    target: DeploymentTarget,
    model: Ssl,
    /// `+0x24`, −48.0 at construction.
    comp: f32,
    /// `+0x28`, 1.0 at construction.
    level: f32,
    /// `+0x2c`, 1.0 at construction.
    attack: f32,
}

impl SslBusComp {
    /// `None` for a model the stock law does not know (it sends nothing).
    pub(super) fn new(identity: &ModelIdentity) -> Option<Self> {
        let model = match identity.model_id {
            5017 => Ssl::Plini,
            5022 => Ssl::Slo,
            5023 => Ssl::Nolly,
            5030 => Ssl::Henson,
            _ => return None,
        };
        Some(Self {
            target: identity.target,
            model,
            comp: -48.0,
            level: 1.0,
            attack: 1.0,
        })
    }

    /// `0x569990`: every derived value, in stock order. All f32; `fma` and
    /// `fms` are fused as in the image.
    fn send(&self, out: &mut Vec<DspWrite>) {
        let (comp, level, attack) = (self.comp, self.level, self.attack);
        let gain = |db| to_normalized(db, -48.0, 48.0, 1.0);
        let attack_ms = |seconds: f32| to_normalized(seconds * 1000.0, 0.1, 30.0, 0.6);
        let release = |seconds| to_normalized(seconds, 0.01, 1.2, 0.6);
        // (COMP − 0.65) / 0.35 above the 0.65 knee.
        let knee = (comp - 0.65) / 0.35;
        let threshold = comp * -48.0;
        let writes: &[(u32, f32)] = match self.model {
            Ssl::Plini | Ssl::Nolly => {
                let makeup = gain(threshold.mul_add(-0.25, level * 24.0) - 12.0);
                let threshold = to_normalized(threshold + 10.0, -48.0, 12.0, 1.0);
                if self.model == Ssl::Plini {
                    &[(0, threshold), (4, makeup)]
                } else {
                    let (ratio, attack) = if attack == 1.0 {
                        (2.0, 12.0)
                    } else {
                        (1.0, 0.7)
                    };
                    &[
                        (0, threshold),
                        (4, makeup),
                        (1, to_normalized(ratio, 0.0, 2.0, 1.0)),
                        (2, to_normalized(attack, 0.1, 30.0, 0.6)),
                    ]
                }
            }
            Ssl::Slo => {
                let seconds = if comp > 0.65 {
                    (-knee).mul_add(0.15, 0.25)
                } else {
                    0.25
                };
                let attack = if attack > 0.5 {
                    0.003
                } else if comp < 0.65 {
                    0.02
                } else {
                    (-knee).mul_add(0.01, 0.02)
                };
                &[
                    (0, to_normalized(threshold, -48.0, 12.0, 1.0)),
                    (3, release(seconds)),
                    (4, gain((comp * comp).mul_add(8.0, (level - 0.5) * 24.0))),
                    (2, attack_ms(attack)),
                    (6, gain(comp * 12.0)),
                ]
            }
            Ssl::Henson => {
                let seconds = if comp > 0.65 {
                    (-knee).mul_add(0.15, 0.3)
                } else {
                    0.3
                };
                let curve = if attack > 0.0 {
                    &HENSON_FAST
                } else {
                    &HENSON_SLOW
                };
                let offset = curve.to_display(comp).unwrap_or(0.0);
                &[
                    (0, to_normalized(threshold, -48.0, 12.0, 1.0)),
                    (3, release(seconds)),
                    (4, gain((level - 0.5).mul_add(24.0, offset))),
                    (2, attack_ms(if attack > 0.5 { 0.008 } else { 0.03 })),
                    (6, gain(comp * 12.0)),
                ]
            }
        };
        out.extend(
            writes
                .iter()
                .map(|&(index, value)| DspWrite::parameter(self.target, index, value)),
        );
    }
}

impl Driver for SslBusComp {
    fn kind(&self) -> DriverKind {
        DriverKind::SslBusComp
    }

    /// Every mode; index 8 is the GAIN REDUCTION meter.
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let hidden: &[u32] = match self.model {
            Ssl::Plini => &[0, 4, 8],
            Ssl::Slo | Ssl::Henson => &[0, 2, 3, 4, 6, 8],
            Ssl::Nolly => &[0, 1, 2, 4, 8],
        };
        match index {
            COMP => self.comp = value,
            LEVEL => self.level = value,
            ATTACK if self.model != Ssl::Plini => self.attack = value,
            index => return !hidden.contains(&index),
        }
        self.send(out);
        false
    }
}

const INTENSITY: u32 = 2;
/// MUTE PROC: 1.0 before a reload, 0.0 after it.
const MUTE_PROC: u32 = 5;
/// `QTimer::start(20)` at 0x5622a0.
const HORIZON_RELOAD_MS: u64 = 20;

/// Buffers 0..=3 per INTENSITY (`0xb903b0` pointer and length tables).
const HORIZON: [[StockPayload; 4]; 2] = [
    [
        payload!(0x00ba_26d4 => 0x3c),
        payload!(0x00b9_0410 => 0x6e44),
        payload!(0x00b9_e098 => 0x2200),
        payload!(0x00ba_2498 => 0x100),
    ],
    [
        payload!(0x00ba_2698 => 0x3c),
        payload!(0x00b9_7254 => 0x6e44),
        payload!(0x00ba_0298 => 0x2200),
        payload!(0x00ba_2598 => 0x100),
    ],
];

pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    HORIZON.into_iter().flatten()
}

#[derive(Debug)]
pub(super) struct Horizon {
    target: DeploymentTarget,
    /// `+0x28`: `lroundf(INTENSITY)`.
    intensity: i32,
    /// `+0x2c`, set at construction.
    dirty: bool,
    timer: Timer,
}

impl Horizon {
    pub(super) fn new(identity: &ModelIdentity) -> Self {
        Self {
            target: identity.target,
            intensity: 0,
            dirty: true,
            timer: Timer::default(),
        }
    }

    /// `+0x48` and the timer slot: upload the selected set (`0x5623e4`) when
    /// the timer is idle and a change is pending, then unmute.
    fn reload(&mut self, out: &mut Vec<DspWrite>) {
        if self.timer.active || !self.dirty {
            return;
        }
        // Stock indexes its tables with the raw value; 0 and 1 are the only
        // sets (owned clamp).
        let set = &HORIZON[usize::from(self.intensity > 0)];
        out.extend(
            (0..)
                .zip(set)
                .map(|(buffer, &payload)| DspWrite::StockData {
                    target: self.target,
                    buffer,
                    payload,
                }),
        );
        out.push(DspWrite::parameter(self.target, MUTE_PROC, 0.0));
        self.dirty = false;
    }
}

impl Driver for Horizon {
    fn kind(&self) -> DriverKind {
        DriverKind::HorizonClarityComp
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            INTENSITY => {
                // `lroundf`; Rust saturates and maps NaN to 0 (owned).
                let intensity = value.round() as i32;
                if intensity != self.intensity {
                    self.intensity = intensity;
                    self.dirty = true;
                    if mode != ParamSetMode::Push && !self.timer.active {
                        out.push(DspWrite::parameter(self.target, MUTE_PROC, 1.0));
                        self.timer.start(HORIZON_RELOAD_MS);
                    }
                }
                true
            }
            // MUTE PROC and the GAIN REDUCTION meter.
            5 | 6 => false,
            _ => true,
        }
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
    use super::super::amps::tests::identity;
    use super::*;

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

    /// `0x569990` per model, with values worked from the f32 image law:
    /// COMP 0.8 crosses the 0.65 knee; Henson interpolates its fast curve.
    #[test]
    fn ssl_laws_follow_the_stock_formulas() {
        let mut out = Vec::new();
        let mut plini = SslBusComp::new(&identity(5017)).unwrap();
        assert!(!plini.update_parameter(COMP, 0.5, ParamSetMode::Push, &mut out));
        // Threshold −24 + 10 = −14 dB of −48..12; makeup 6 + 24 − 12 = 18 dB
        // with the constructor LEVEL 1.0.
        assert_eq!(
            values(&out),
            [(0, 34.0 / 60.0), (4, to_normalized(18.0, -48.0, 48.0, 1.0))]
        );
        assert!(!plini.update_parameter(4, 0.3, ParamSetMode::Push, &mut out));
        assert!(plini.update_parameter(5, 0.3, ParamSetMode::Push, &mut out));

        out.clear();
        let mut slo = SslBusComp::new(&identity(5022)).unwrap();
        slo.level = 0.5;
        slo.attack = 0.0;
        slo.update_parameter(COMP, 0.8, ParamSetMode::Live, &mut out);
        let knee = (0.8_f32 - 0.65) / 0.35;
        assert_eq!(
            values(&out),
            [
                (0, to_normalized(0.8 * -48.0, -48.0, 12.0, 1.0)),
                (
                    3,
                    to_normalized((-knee).mul_add(0.15, 0.25), 0.01, 1.2, 0.6)
                ),
                (4, to_normalized(0.8_f32 * 0.8 * 8.0, -48.0, 48.0, 1.0)),
                (
                    2,
                    to_normalized((-knee).mul_add(0.01, 0.02) * 1000.0, 0.1, 30.0, 0.6)
                ),
                (6, to_normalized(0.8 * 12.0, -48.0, 48.0, 1.0)),
            ]
        );

        out.clear();
        let mut henson = SslBusComp::new(&identity(5030)).unwrap();
        henson.level = 0.75;
        henson.update_parameter(COMP, 0.625, ParamSetMode::Recall, &mut out);
        // Fast curve between (0.5, 0) and (0.75, 7): 3.5 dB, plus 6 dB LEVEL.
        assert_eq!(values(&out)[2], (4, to_normalized(9.5, -48.0, 48.0, 1.0)));
        assert_eq!(
            values(&out)[3],
            (2, to_normalized(0.008_f32 * 1000.0, 0.1, 30.0, 0.6)),
            "ATTACK 1.0 at construction is fast"
        );
        assert!(!henson.update_parameter(3, 0.5, ParamSetMode::Live, &mut out));
        assert!(henson.update_parameter(1, 0.5, ParamSetMode::Live, &mut out));
    }

    /// `0x56220c`/`0x56252c`: the push uploads the selected set once; a live
    /// INTENSITY change mutes MUTE PROC and reloads 20 ms later.
    #[test]
    fn horizon_intensity_reloads_through_a_muted_timer() {
        let mut horizon = Horizon::new(&identity(5029));
        let mut out = Vec::new();
        assert!(horizon.update_parameter(INTENSITY, 0.0, ParamSetMode::Push, &mut out));
        horizon.parameters_pushed(&mut out);
        assert_eq!(out.len(), 5);
        assert!(
            matches!(out[0], DspWrite::StockData { buffer: 0, payload, .. } if payload == HORIZON[0][0])
        );
        out.clear();
        horizon.parameters_pushed(&mut out);
        assert!(out.is_empty(), "nothing pending");
        assert!(horizon.update_parameter(INTENSITY, 1.0, ParamSetMode::Live, &mut out));
        horizon.parameters_pushed(&mut out);
        assert_eq!(
            values(&out),
            [(MUTE_PROC, 1.0)],
            "the timer holds the upload"
        );
        assert_eq!(
            horizon.take_timer(),
            Some(std::time::Duration::from_millis(20))
        );
        out.clear();
        horizon.timer_elapsed(&mut out);
        assert!(
            matches!(out[3], DspWrite::StockData { buffer: 3, payload, .. } if payload == HORIZON[1][3])
        );
        assert_eq!(values(&out[4..]), [(MUTE_PROC, 0.0)]);
        assert!(!horizon.update_parameter(5, 1.0, ParamSetMode::Live, &mut out));
    }

    /// The Horizon and Rabea buffer sets match their stock pointer and
    /// length tables (`0xb903b0`, `0xba27b0`): buffer 0 (0x3c bytes) has a
    /// pointer per set, every later buffer a pointer per set, then a length
    /// per set (Horizon buffer 3 counts words). Nothing is printed or saved.
    #[test]
    #[ignore = "requires the local pinned ZenUI corpus"]
    fn horizon_and_rabea_sets_match_their_pointer_tables() {
        use crate::setup::{ElfTables, Segment, StockTables};

        fn check(sets: [&[StockPayload]; 2], table: &[u32], word_lengths: &[usize]) {
            for (set, payloads) in sets.into_iter().enumerate() {
                let first = payloads[0].segments()[0];
                assert_eq!((first.va, first.len), (table[set], 0x3c));
                for (buffer, payload) in payloads.iter().enumerate().skip(1) {
                    let row = 2 + 4 * (buffer - 1);
                    let scale = if word_lengths.contains(&buffer) { 4 } else { 1 };
                    let segment = payload.segments()[0];
                    assert_eq!(segment.va, table[row + set], "buffer {buffer}");
                    assert_eq!(segment.len, table[row + 2 + set] * scale, "buffer {buffer}");
                }
            }
        }

        let Some(path) = crate::corpus("rootfs/opt/neuraldsp/ZenUI") else {
            return;
        };
        let image = ElfTables::new(std::fs::read(path).unwrap()).unwrap();
        let table = |va: u32, words: u32| -> Vec<u32> {
            let segment: &'static [Segment] = Box::leak(Box::new([Segment { va, len: words * 4 }]));
            let bytes = image.read(StockPayload::new(segment)).unwrap();
            super::super::reload::words(&bytes).collect()
        };
        check([&HORIZON[0], &HORIZON[1]], &table(0x00b9_03b0, 14), &[3]);
        let rabea = super::super::drives::RABEA;
        check([&rabea[0], &rabea[1]], &table(0x00ba_27b0, 10), &[]);
    }
}
