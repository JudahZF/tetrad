//! Push-all steps before the grid blocks (VA `0x583078`): the compiler's queued router writes
//! (`0x582e80`), then per split/mix row pair (rows 0 and 2) the splitter (`0x51e2b8`) and the mixer
//! (`0x511548`).
//!
//! The splitter and mixer have no DSP model of their own. Their values are
//! converted and written to routers that the compiler placed for the pair.

use tetrad_dsp_protocol::DeploymentTarget;
use tetrad_dsp_protocol::requests::BypassState;

use super::super::DspWrite;
use super::gain_db;

/// One value of a compiler-queued router write (record byte `+4`: 0 =
/// `0x03`, else `0x02`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RouterValue {
    Int(u32),
    Float(f32),
}

/// One record of the compiler router queue (map `+0xc40`, 12-byte records).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouterWrite {
    pub index: u32,
    pub value: RouterValue,
}

/// The writes the compiler queued for one router while it placed it
/// (VAs `0x87d260`, `0x87d3b4`, `0x87d568`, `0x87d778`, …), in queue order.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterSetup {
    pub target: DeploymentTarget,
    pub writes: Vec<RouterWrite>,
}

/// Stock step `0x582e80`: routers in map order (key `core·30 + slot`), each
/// router's records in queue order. Floats go through the bulk queue
/// (`0x517010`); ints go out at once (`0x511318`). Stock then clears the map.
pub fn push_routers(routers: &[RouterSetup], out: &mut Vec<DspWrite>) {
    let mut order: Vec<&RouterSetup> = routers.iter().collect();
    order.sort_by_key(|router| (router.target.core as u8, router.target.slot.index()));
    for router in order {
        let target = router.target;
        for write in &router.writes {
            out.push(match write.value {
                RouterValue::Int(value) => DspWrite::IntParameter {
                    target,
                    index: write.index,
                    value,
                },
                RouterValue::Float(value) => DspWrite::parameter(target, write.index, value),
            });
        }
    }
}

/// Splitter (model 10004) TYPE: `lroundf(v / 0.5)` with the parameter's
/// step size (3 steps, VA `0x5cc994`). Other results match no branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitterType {
    Balance,
    AB,
    Crossover,
    Other,
}

impl SplitterType {
    pub fn from_value(value: f32) -> Self {
        // `f32::round` rounds half away from zero, as `lroundf` does.
        match (value / 0.5).round() {
            0.0 => Self::Balance,
            1.0 => Self::AB,
            2.0 => Self::Crossover,
            _ => Self::Other,
        }
    }
}

/// Balance law of `0x582278`: `fma(x, x·−0.8317831, x·1.831783)`.
fn balance(x: f32) -> f32 {
    x.mul_add(x * -0.831_783_1, x * 1.831_783)
}

/// Level law of the splitter A/B and mixer routers: `v = 0` sends 0.0
/// (`vcmp`/`vmoveq` at `0x582450`, `0x582384`, `0x581de0`), else range 52.
fn router_level(value: f32) -> f32 {
    if value == 0.0 {
        0.0
    } else {
        gain_db(value, 52.0)
    }
}

/// The junction model a value belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JunctionSite {
    /// Model 10004 (`0x582278`).
    Splitter,
    /// Model 11000 (`0x581f98`).
    Mixer,
}

/// The compiler's routers for one split/mix row pair. `None` = the router's
/// "placed" byte (`+5`) is clear. The only home of the fused junction laws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Junction {
    /// Split router, compiler `+0x5a4` (VA `0x87a1cc`).
    pub split: Option<DeploymentTarget>,
    /// Merge router, compiler `+0x5c4` (VA `0x87a20c`).
    pub merge: Option<DeploymentTarget>,
    /// Crossover heads of the pair's two rows, compiler `+0x674` (VA
    /// `0x87a1ec`, rows r and r+1).
    pub heads: [Option<DeploymentTarget>; 2],
}

impl Junction {
    /// Stock splitter push (VA `0x51e2b8`). `values` is the splitter's
    /// runtime list in index order with expression values substituted;
    /// stock reads TYPE (index 0) from the scene value (`0x51dffc`), so the
    /// caller passes that at index 0. The last index is never written.
    pub fn push_splitter(&self, values: &[f32], out: &mut Vec<DspWrite>) {
        let kind = values.first().map_or(SplitterType::Balance, |&value| {
            SplitterType::from_value(value)
        });
        // `0x5821a4`: crossover opens the split router fully, and the heads
        // run only for crossover. Stock writes the split router even when it
        // is not placed; a missing router is skipped here.
        if kind == SplitterType::Crossover
            && let Some(split) = self.split
        {
            out.push(DspWrite::parameter(split, 3, 1.0));
            out.push(DspWrite::parameter(split, 5, 1.0));
        }
        let state = if kind == SplitterType::Crossover {
            BypassState::Engaged
        } else {
            BypassState::Bypassed
        };
        for head in self.heads.into_iter().flatten() {
            out.push(DspWrite::Bypass {
                target: head,
                state,
            });
        }
        let last = values.len().saturating_sub(1);
        for (index, &value) in values.iter().enumerate().take(last) {
            self.splitter_value(kind, index as u32, value, out);
        }
    }

    /// `0x582278` for one splitter value.
    fn splitter_value(&self, kind: SplitterType, index: u32, value: f32, out: &mut Vec<DspWrite>) {
        let Some(split) = self.split else {
            return;
        };
        let [master, slave] = self.heads;
        let mut heads = |index, master_value, slave_value| {
            if let Some(head) = master {
                out.push(DspWrite::parameter(head, index, master_value));
            }
            if let Some(head) = slave {
                out.push(DspWrite::parameter(head, index, slave_value));
            }
        };
        match (index, kind) {
            (1, _) => out.push(DspWrite::parameter(split, 6, value)),
            (2, SplitterType::Balance) => {
                out.push(DspWrite::parameter(split, 5, balance(1.0 - value)));
                out.push(DspWrite::parameter(split, 3, balance(value)));
            }
            (3, SplitterType::AB) => out.push(DspWrite::parameter(split, 5, router_level(value))),
            (4, SplitterType::AB) => out.push(DspWrite::parameter(split, 3, router_level(value))),
            (5, SplitterType::Crossover) => heads(0, value, value),
            (6, SplitterType::Crossover) => heads(1, value, 1.0 - value),
            _ => {}
        }
    }

    /// Stock mixer push (VA `0x511548` → `0x581f98`). `values` is the
    /// mixer's runtime list as for [`Self::push_splitter`]. The last index
    /// is not written; while it is above 0.5 path B is muted (`0x514378`,
    /// the value the edit path `0x51e4d8` tests) and LEVEL B is written as
    /// `10^−106`.
    pub fn push_mixer(&self, values: &[f32], out: &mut Vec<DspWrite>) {
        for index in 0..values.len().saturating_sub(1) {
            self.mixer_value(index as u32, values, out);
        }
    }

    /// `0x581f98` for one mixer value; `values` ends with the BYPASS value.
    fn mixer_value(&self, index: u32, values: &[f32], out: &mut Vec<DspWrite>) {
        let (Some(merge), Some((&mute_b, values))) = (self.merge, values.split_last()) else {
            return;
        };
        let Some(&value) = values.get(index as usize) else {
            return;
        };
        let (dsp_index, dsp_value) = match index {
            0 => (7, router_level(value)),
            1 => (8, value),
            2 if mute_b > 0.5 => (9, 10_f32.powf(-106.0)),
            2 => (9, router_level(value)),
            3 => (10, value),
            4 => (11, value),
            5 => (6, router_level(value)),
            6 => (12, value),
            _ => return,
        };
        out.push(DspWrite::parameter(merge, dsp_index, dsp_value));
    }

    /// A live junction edit (`0x51e4d8` cases 0x2714, 0x2af8): splitter
    /// TYPE re-runs the whole splitter push, other splitter values write
    /// their router or heads; the mixer's last index (BYPASS) rewrites LEVEL
    /// B (`0x514378`), other mixer values write the merge router. `values`
    /// is the model's runtime list as for the pushes.
    pub fn edit(&self, site: JunctionSite, index: u32, values: &[f32], out: &mut Vec<DspWrite>) {
        match site {
            JunctionSite::Splitter if index == 0 => self.push_splitter(values, out),
            JunctionSite::Splitter => {
                let kind = values.first().map_or(SplitterType::Balance, |&value| {
                    SplitterType::from_value(value)
                });
                if let Some(&value) = values.get(index as usize) {
                    self.splitter_value(kind, index, value, out);
                }
            }
            JunctionSite::Mixer if index as usize + 1 == values.len() => {
                self.mixer_value(2, values, out);
            }
            JunctionSite::Mixer => self.mixer_value(index, values, out),
        }
    }
}

#[cfg(test)]
mod tests {
    use tetrad_dsp_protocol::{DspCore, ModuleSlot};

    use super::*;

    fn target(core: DspCore, slot: u32) -> DeploymentTarget {
        DeploymentTarget {
            core,
            slot: ModuleSlot::try_from(slot).unwrap(),
        }
    }

    /// `0x582e80` walks the router map by `core·30 + slot`, not insertion order.
    #[test]
    fn routers_go_out_in_map_order_with_their_record_types() {
        let late = target(DspCore::Core1, 2);
        let early = target(DspCore::Core0, 29);
        let routers = [
            RouterSetup {
                target: late,
                writes: vec![RouterWrite {
                    index: 9,
                    value: RouterValue::Float(1.0),
                }],
            },
            RouterSetup {
                target: early,
                writes: vec![
                    RouterWrite {
                        index: 0,
                        value: RouterValue::Int(3),
                    },
                    RouterWrite {
                        index: 3,
                        value: RouterValue::Float(1.0),
                    },
                ],
            },
        ];
        let mut out = Vec::new();
        push_routers(&routers, &mut out);
        assert_eq!(
            out,
            [
                DspWrite::IntParameter {
                    target: early,
                    index: 0,
                    value: 3,
                },
                DspWrite::parameter(early, 3, 1.0),
                DspWrite::parameter(late, 9, 1.0),
            ]
        );
    }

    /// Crossover setup (`0x5821a4`) precedes the values; the last value is
    /// skipped; MODE writes `v` and `1 − v` to the two heads.
    #[test]
    fn crossover_splitter_opens_the_split_router_and_drives_both_heads() {
        let split = target(DspCore::Core0, 1);
        let heads = [target(DspCore::Core0, 2), target(DspCore::Core2, 3)];
        let junction = Junction {
            split: Some(split),
            merge: None,
            heads: heads.map(Some),
        };
        // TYPE, STEREO, BALANCE, LEVEL A, LEVEL B, FREQUENCY, MODE, MUTE, BYPASS
        let values = [1.0, 0.0, 0.5, 0.5, 0.5, 0.4, 0.25, 0.0, 1.0];
        let mut out = Vec::new();
        junction.push_splitter(&values, &mut out);
        let engaged = |target| DspWrite::Bypass {
            target,
            state: BypassState::Engaged,
        };
        assert_eq!(
            out,
            [
                DspWrite::parameter(split, 3, 1.0),
                DspWrite::parameter(split, 5, 1.0),
                engaged(heads[0]),
                engaged(heads[1]),
                DspWrite::parameter(split, 6, 0.0),
                DspWrite::parameter(heads[0], 0, 0.4),
                DspWrite::parameter(heads[1], 0, 0.4),
                DspWrite::parameter(heads[0], 1, 0.25),
                DspWrite::parameter(heads[1], 1, 0.75),
            ]
        );
    }

    /// Balance law bits (VA `0x582320`: two products, one fused add).
    #[test]
    fn balance_splitter_uses_the_fused_balance_law() {
        let split = target(DspCore::Core1, 0);
        let junction = Junction {
            split: Some(split),
            ..Junction::default()
        };
        let mut out = Vec::new();
        junction.push_splitter(&[0.0, 0.0, 0.3, 0.0], &mut out);
        let x = 0.7_f32;
        let a = x.mul_add(x * -0.831_783_1, x * 1.831_783);
        assert_eq!(out[1], DspWrite::parameter(split, 5, a));
        assert_eq!(out[2], DspWrite::parameter(split, 3, balance(0.3)));
        assert_eq!(1.831_783_f32.to_bits(), 0x3fea_77de);
        assert_eq!((-0.831_783_1_f32).to_bits(), 0xbf54_efbd);
    }

    /// LEVEL A/B at 0 send 0.0 (stock `vcmp`/`vmoveq`), not `10^(−40·0.05)`.
    #[test]
    fn ab_splitter_level_zero_sends_silence() {
        let split = target(DspCore::Core0, 1);
        let junction = Junction {
            split: Some(split),
            ..Junction::default()
        };
        // TYPE = AB, STEREO, BALANCE, LEVEL A, LEVEL B, last (never written)
        let mut out = Vec::new();
        junction.push_splitter(&[0.5, 0.0, 0.0, 0.0, 0.0, 0.0], &mut out);
        assert_eq!(
            out,
            [
                DspWrite::parameter(split, 6, 0.0),
                DspWrite::parameter(split, 5, 0.0),
                DspWrite::parameter(split, 3, 0.0),
            ]
        );
    }

    /// `0x581f98` remaps mixer indices; a set last value mutes path B.
    #[test]
    fn mixer_remaps_indices_and_mutes_path_b() {
        let merge = target(DspCore::Core3, 7);
        let junction = Junction {
            merge: Some(merge),
            ..Junction::default()
        };
        let mut out = Vec::new();
        junction.push_mixer(&[0.0, 0.5, 0.769_230_8, 0.5, 1.0, 0.5, 0.0, 1.0], &mut out);
        let indices: Vec<u32> = out
            .iter()
            .map(|write| match write {
                DspWrite::Parameter { index, .. } => *index,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(indices, [7, 8, 9, 10, 11, 6, 12]);
        assert_eq!(out[0], DspWrite::parameter(merge, 7, 0.0));
        assert_eq!(out[2], DspWrite::parameter(merge, 9, 0.0));
    }

    /// `0x51e4d8` junction cases: splitter TYPE re-runs the whole push (here
    /// a crossover), another value writes only its own router; the mixer's
    /// BYPASS index rewrites LEVEL B, muted while BYPASS is set.
    #[test]
    fn junction_edits_follow_the_stock_cases() {
        let split = target(DspCore::Core1, 3);
        let merge = target(DspCore::Core1, 4);
        let head = target(DspCore::Core1, 5);
        let junction = Junction {
            split: Some(split),
            merge: Some(merge),
            heads: [Some(head), None],
        };
        let crossover = [1.0, 0.0, 0.5, 0.5, 0.5, 0.25, 0.5, 0.0, 0.0];
        let mut type_edit = Vec::new();
        junction.edit(JunctionSite::Splitter, 0, &crossover, &mut type_edit);
        let mut push = Vec::new();
        junction.push_splitter(&crossover, &mut push);
        assert_eq!(type_edit, push);
        let mut frequency = Vec::new();
        junction.edit(JunctionSite::Splitter, 5, &crossover, &mut frequency);
        assert_eq!(frequency, [DspWrite::parameter(head, 0, 0.25)]);
        let mixer = [0.5, 0.5, 0.75, 0.5, 0.5, 0.5, 0.5, 0.0, 1.0];
        let mut bypass = Vec::new();
        junction.edit(JunctionSite::Mixer, 8, &mixer, &mut bypass);
        assert_eq!(bypass, [DspWrite::parameter(merge, 9, 0.0)]);
    }
}
