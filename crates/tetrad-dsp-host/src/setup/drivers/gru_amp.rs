//! GRUAmpDriver: constructor VA `0x564f90`, `+0x30` `0x563c18`; every other hook is the base one.
//!
//! The constructor sends four ReceiveData buffers (`0x51965c`, wait 1):
//! 0 = the GRU topology record (a 0x38-byte stock table for 42, 1154, 1155
//! and 1156, copied through `0x563db0` and `0x563e78`; words built in code
//! for 1171, 49 and 1184),
//! 1 and 2 = one table each (`0x563f7c`, `0x5641f8`), 3 = parts from
//! `0x564630`, `0x564464`, `0x56475c` and per-model extras (`0x564cfc`),
//! sent only when not empty.

use std::sync::Arc;

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::tables::{Segment, StockPayload};
use super::super::{DspWrite, ModelIdentity, ParamSetMode, SetupError};
use super::catalog::to_display;
use super::{Driver, DriverKind};

macro_rules! table {
    ($($va:expr => $len:expr),+ $(,)?) => {
        StockPayload::new(&[$(Segment { va: $va, len: $len }),+])
    };
}

/// Buffer 0: a stock table, or words the constructor builds (as stored).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Topology {
    Table(StockPayload),
    Words(&'static [u32]),
}

/// Constructor writes per model: buffer 0, buffer 1, buffer 2, buffer 3.
fn buffers(model_id: u32) -> Option<(Topology, StockPayload, StockPayload, StockPayload)> {
    use Topology::{Table, Words};
    Some(match model_id {
        42 => (
            Table(table!(0x00bb_8bf0 => 0x38)),
            table!(0x00be_1ea8 => 0x5ea4),
            table!(0x00c1_e01c => 0x940),
            table!(0x00c2_53e8 => 0x58),
        ),
        49 => (
            // 1.8 and −7.44 as f32 bits (`0x5651f8`).
            Words(&[
                1,
                2,
                8,
                32,
                1,
                2,
                32,
                1,
                0,
                0,
                0,
                1,
                0x3fe6_6666,
                0xc0ee_147b,
                0x80,
            ]),
            table!(0x00bf_e890 => 0x6e44),
            table!(0x00c2_0ddc => 0x2200),
            table!(0x00c2_54b8 => 0x14, 0x00c2_56e8 => 0x200, 0x00c2_68e8 => 0x10),
        ),
        1154 => (
            Table(table!(0x00bb_8b48 => 0x38)),
            table!(0x00bb_8cdc => 0xdb44),
            table!(0x00c1_c21c => 0xa40),
            table!(0x00c2_54a0 => 0x14, 0x00c2_52e0 => 0x58),
        ),
        1155 => (
            Table(table!(0x00bb_8b80 => 0x38)),
            table!(0x00bc_6820 => 0xdb44),
            table!(0x00c1_cc5c => 0xa00),
            table!(0x00c2_5338 => 0x58),
        ),
        1156 => (
            Table(table!(0x00bb_8bb8 => 0x38)),
            table!(0x00bd_4364 => 0xdb44),
            table!(0x00c1_d65c => 0x9c0),
            table!(0x00c2_5390 => 0x58),
        ),
        1171 => (
            Words(&[6, 3, 16, 32, 1, 2, 32, 6, 12, 4, 0, 0, 0, 0, 0x80]),
            table!(0x00be_7d4c => 0x16b44),
            table!(0x00c1_e95c => 0x2480),
            table!(0x00c2_5440 => 0x60, 0x00c2_54e8 => 0x200),
        ),
        1184 => (
            Words(&[3, 3, 16, 32, 1, 2, 32, 3, 0, 0, 0, 1, 0, 0, 0x400]),
            table!(0x00c0_56d8 => 0x16b44),
            table!(0x00c2_2fe0 => 0x2300),
            table!(
                0x00c2_54d0 => 0x14,
                0x00c2_58e8 => 0x1000,
                0x00a5_2c00 => 0x100,
                0x00a5_2d00 => 0xc0,
            ),
        ),
        _ => return None,
    })
}

const MODELS: [u32; 7] = [42, 49, 1154, 1155, 1156, 1171, 1184];

/// Every GRUAmp table, for [`super::stock_payloads`].
pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    MODELS
        .into_iter()
        .filter_map(buffers)
        .flat_map(|(topology, one, two, three)| {
            let table = match topology {
                Topology::Table(payload) => Some(payload),
                Topology::Words(_) => None,
            };
            table.into_iter().chain([one, two, three])
        })
}

/// The only model whose `+0x30` converts values (`0x563c18`).
const CONVERTING_MODEL: u32 = 1184;

#[derive(Debug)]
pub(super) struct GruAmp {
    target: DeploymentTarget,
    model_id: u32,
}

impl GruAmp {
    pub(super) fn new(
        identity: &ModelIdentity,
        out: &mut Vec<DspWrite>,
    ) -> Result<Self, SetupError> {
        let ModelIdentity {
            target, model_id, ..
        } = *identity;
        let (topology, one, two, three) =
            buffers(model_id).ok_or(SetupError::UnsupportedDriver {
                model_id,
                driver: DriverKind::GruAmp,
            })?;
        out.push(match topology {
            Topology::Table(payload) => DspWrite::StockData {
                target,
                buffer: 0,
                payload,
            },
            Topology::Words(words) => DspWrite::Data {
                target,
                buffer: 0,
                payload: words
                    .iter()
                    .flat_map(|word| word.to_le_bytes())
                    .collect::<Arc<[u8]>>(),
            },
        });
        for (buffer, payload) in [(1, one), (2, two), (3, three)] {
            out.push(DspWrite::StockData {
                target,
                buffer,
                payload,
            });
        }
        Ok(Self { target, model_id })
    }
}

/// `0x60749c`: `powf(10, dB / 20)`.
fn db_to_gain(decibels: f32) -> f32 {
    10_f32.powf(decibels / 20.0)
}

impl Driver for GruAmp {
    fn kind(&self) -> DriverKind {
        DriverKind::GruAmp
    }

    /// 1184 only: indices 0..=2 go out as display dB (−60..12, per-index
    /// skew), 4 and 5 as linear gains (−12..12 dB; −40..6 dB skew 2, and 0
    /// stays 0). Every other index and model passes.
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        if self.model_id != CONVERTING_MODEL {
            return true;
        }
        let level = |skew_bits| to_display(value, -60.0, 12.0, f32::from_bits(skew_bits));
        let converted = match index {
            0 => level(0x4073_50b1),
            1 => level(0x4083_2fec),
            2 => level(0x404d_2a30),
            4 => db_to_gain(to_display(value, -12.0, 12.0, 1.0)),
            5 if value == 0.0 => 0.0,
            5 => db_to_gain(to_display(value, -40.0, 6.0, 2.0)),
            _ => return true,
        };
        out.push(DspWrite::parameter(self.target, index, converted));
        false
    }
}

#[cfg(test)]
mod tests {
    use tetrad_dsp_protocol::{DspCore, ModuleSlot};

    use super::super::super::{BlockBypass, SetupValue};
    use super::*;
    use crate::setup::test_factory::build;

    fn identity(model_id: u32) -> ModelIdentity {
        ModelIdentity {
            target: DeploymentTarget {
                core: DspCore::Core1,
                slot: ModuleSlot::try_from(3).unwrap(),
            },
            model_id,
            category: 1,
            fx_loop_send: None,
        }
    }

    /// Constructor order: buffer 0 (words built in code, little endian), then
    /// stock tables 1, 2 and 3; 1184 converts index 0 and 5 and passes index 3.
    #[test]
    fn gru_1184_sends_code_built_topology_and_converts_levels() {
        let identity = identity(1184);
        let target = identity.target;
        let mut out = Vec::new();
        let mut block = build(identity, &mut out).unwrap();
        let buffers: Vec<u8> = out
            .iter()
            .map(|write| match write {
                DspWrite::Data {
                    buffer, payload, ..
                } => {
                    assert_eq!(payload.len(), 0x3c);
                    assert_eq!(payload[56..60], 0x400_u32.to_le_bytes());
                    *buffer
                }
                DspWrite::StockData { buffer, .. } => *buffer,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(buffers, [0, 1, 2, 3]);
        out.clear();
        let values =
            [0.5_f32, 0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 1.0].map(|v| SetupValue::Float(v.to_bits()));
        block
            .push(&values, BlockBypass::default(), None, &mut out)
            .unwrap();
        let skew = f32::from_bits(0x4073_50b1);
        assert!(out.contains(&DspWrite::parameter(
            target,
            0,
            to_display(0.5, -60.0, 12.0, skew)
        )));
        assert!(out.contains(&DspWrite::parameter(target, 3, 0.5)));
        assert!(out.contains(&DspWrite::parameter(target, 4, 1.0)));
        assert!(out.contains(&DspWrite::parameter(target, 5, 0.0)));
    }

    /// 42 sends its topology from the stock table; other GRU models pass
    /// every value.
    #[test]
    fn gru_42_uses_the_topology_table_and_passes_values() {
        let mut out = Vec::new();
        let mut block = build(identity(42), &mut out).unwrap();
        assert!(matches!(
            out[0],
            DspWrite::StockData { buffer: 0, payload, .. } if payload.len() == 0x38
        ));
        out.clear();
        let values = [0.3_f32; 4].map(|v| SetupValue::Float(v.to_bits()));
        block
            .push(&values, BlockBypass::default(), None, &mut out)
            .unwrap();
        assert_eq!(out.len(), 3);
    }

    /// 1154 copies its topology from the stock table, not from code-built words.
    #[test]
    fn gru_1154_uses_the_stock_topology_table() {
        let mut out = Vec::new();
        build(identity(1154), &mut out).unwrap();
        assert!(matches!(
            out[0],
            DspWrite::StockData { buffer: 0, payload, .. } if payload.len() == 0x38
        ));
    }
}
