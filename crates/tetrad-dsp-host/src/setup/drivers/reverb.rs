//! ReverbDriver: constructor VA `0x55b258`, `sendConstData` `0x55b00c`,
//! `+0x30` `0x55b4bc`; every other hook is the base one.
//!
//! The constructor sends two constant tables (buffers 0 and 1). Seven
//! models send most values in display units, converted with the catalog
//! ranges; five others derive some DSP values from one control with
//! driver-local laws; the rest pass every value through.

use std::sync::LazyLock;

use tetrad_core::conversion::{Breakpoint, Piecewise};
use tetrad_dsp_protocol::DeploymentTarget;

use super::super::tables::{Segment, StockPayload};
use super::super::{DspWrite, ModelIdentity, ParamSetMode, SetupError};
use super::catalog::{CatalogParameter, to_display as display, to_normalized};
use super::{Driver, DriverKind};

macro_rules! payload {
    ($va:expr, $len:expr) => {
        StockPayload::new(&[Segment { va: $va, len: $len }])
    };
}

const SHARED: StockPayload = payload!(0x00a5_4ad8, 0x400);
const BUFFER1: [StockPayload; 8] = [
    payload!(0x00a5_3e18, 0x180),
    payload!(0x00a5_3f98, 0x180),
    payload!(0x00a5_4118, 0x180),
    payload!(0x00a5_4298, 0x180),
    payload!(0x00a5_44d8, 0x180),
    payload!(0x00a5_4658, 0x180),
    payload!(0x00a5_47d8, 0x180),
    payload!(0x00a5_4958, 0x180),
];
/// Model 8008 has its own pair.
const PAIR_8008: [StockPayload; 2] = [payload!(0x00a5_4ed8, 0x100), payload!(0x00a5_4418, 0xc0)];

/// `sendConstData` (VA `0x55b00c`, jump table at `0x55b068`): buffer 0
/// and buffer 1 payloads per model.
fn const_data(model_id: u32) -> Option<[StockPayload; 2]> {
    let buffer1 = match model_id {
        8008 => return Some(PAIR_8008),
        8000 => 0,
        8007 | 8015 | 8016 | 8018 | 8021 => 1,
        8003 => 2,
        8009 => 3,
        8010 | 8023 | 8024 | 8032 | 8037 | 8041 => 4,
        8017 => 5,
        8040 => 6,
        8019 => 7,
        _ => return None,
    };
    Some([SHARED, BUFFER1[buffer1]])
}

/// Every reverb table, for [`super::stock_payloads`].
pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    [SHARED].into_iter().chain(BUFFER1).chain(PAIR_8008)
}

/// Models whose values go out in display units (`0x55b540`).
const CATALOG_MODELS: [u32; 7] = [8015, 8016, 8017, 8018, 8019, 8021, 8040];

/// Model 8017 index 0 curve over the display value; static init `0x55be60`.
static CURVE_8017: LazyLock<Piecewise> = LazyLock::new(|| {
    curve(&[
        (1.0, 2.0),
        (5.25, 2.3),
        (9.0005, 4.4),
        (13.75, 9.0),
        (18.0, 18.0),
    ])
});

/// Model 8032 index 1 curve; constructor fields `+0x24..=+0x38`.
static CURVE_8032: LazyLock<Piecewise> =
    LazyLock::new(|| curve(&[(0.0, 4.0), (0.5, 5.5), (1.0, 8.0)]));

/// Builds the stock piecewise law (`bls` chains at `0x55ba00` and `0x55b8e4`,
/// segment math at `0x55bde4`/`0x55be58`). Above the last point and for NaN,
/// `to_display` yields `None`, which callers map to 0.0.
fn curve(points: &[(f32, f32)]) -> Piecewise {
    let points = points.iter().map(|&(x, y)| Breakpoint { x, y }).collect();
    Piecewise::new(points).expect("stock breakpoints are finite and at least two")
}

/// `0x55b47c`: display in one range, normalized in another.
fn rescale(value: f32, from: (f32, f32, f32), to: (f32, f32, f32)) -> f32 {
    to_normalized(display(value, from.0, from.1, from.2), to.0, to.1, to.2)
}

/// `v < 0.6 → v·1.25`, else (and NaN) `fma(v, 0.625, 0.375)`.
fn knee(value: f32) -> f32 {
    if value < 0.6 {
        value * 1.25
    } else {
        value.mul_add(0.625, 0.375)
    }
}

/// `fma(v, 0.98, 0.02)`: the shared size control of 8019, 8024 and 8037.
fn size(value: f32) -> f32 {
    value.mul_add(0.98, 0.02)
}

const SMALL: (f32, f32, f32) = (20.0, 800.0, 0.6);
const PERCENT: (f32, f32, f32) = (0.0, 100.0, 1.0);

#[derive(Debug)]
pub(super) struct Reverb {
    target: DeploymentTarget,
    model_id: u32,
    catalog: Vec<CatalogParameter>,
}

impl Reverb {
    /// Constructor. The catalog models convert with `catalog`; an index past
    /// it uses the stock metadata default.
    pub(super) fn new(
        identity: &ModelIdentity,
        catalog: &[CatalogParameter],
        out: &mut Vec<DspWrite>,
    ) -> Result<Self, SetupError> {
        let ModelIdentity {
            target, model_id, ..
        } = *identity;
        let [buffer0, buffer1] = const_data(model_id).ok_or(SetupError::UnsupportedDriver {
            model_id,
            driver: DriverKind::Reverb,
        })?;
        let catalog = if CATALOG_MODELS.contains(&model_id) {
            catalog.to_vec()
        } else {
            Vec::new()
        };
        for (buffer, payload) in [(0, buffer0), (1, buffer1)] {
            out.push(DspWrite::StockData {
                target,
                buffer,
                payload,
            });
        }
        Ok(Self {
            target,
            model_id,
            catalog,
        })
    }

    /// The display-unit path of the catalog models (`0x55b550..0x55b6c4`).
    fn catalog_value(&self, index: u32, value: f32, out: &mut Vec<DspWrite>) -> bool {
        let send = |out: &mut Vec<DspWrite>, index, value| {
            out.push(DspWrite::parameter(self.target, index, value));
        };
        // Owned: an index past the catalog uses the stock metadata default.
        let range = self
            .catalog
            .get(index as usize)
            .unwrap_or(&CatalogParameter::DEFAULT);
        let mut shown = display(value, range.min, range.max, range.skew);
        if matches!(index, 25 | 26) {
            shown = value;
        }
        if matches!(index, 6 | 7 | 11 | 15 | 19 | 23) {
            shown = shown.round();
        }
        match (self.model_id, index) {
            (8016 | 8019, 5) => shown = display(value, 1000.0, 10000.0, 0.6),
            (8016 | 8019, 4) => shown = display(value, 50.0, 700.0, 1.0),
            (8016, 0) => shown = display(value, 0.25, 10.0, 1.0),
            (8017, 0) => shown = CURVE_8017.to_display(shown).unwrap_or(0.0),
            (8018, 5) => shown = display(value, 2000.0, 10000.0, 0.6),
            (8019, 26) => shown = knee(value),
            (8019, 1) => return false,
            (8019, 0) => {
                let size = size(value);
                send(out, 1, size.mul_add(49.0, 1.0));
                send(out, 0, size.mul_add(7000.0, 50.0) * 0.001);
                return false;
            }
            (8021, 22) => shown = display(value, -38.0, -13.0, 1.0),
            (8021, 16) => shown = display(value, 100.0, 1000.0, 1.0),
            _ => {}
        }
        send(out, index, shown);
        false
    }
}

impl Driver for Reverb {
    fn kind(&self) -> DriverKind {
        DriverKind::Reverb
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        if CATALOG_MODELS.contains(&self.model_id) {
            return (index == 3 || index > 26) || self.catalog_value(index, value, out);
        }
        let target = self.target;
        let mut send = |index, value| out.push(DspWrite::parameter(target, index, value));
        match (self.model_id, index) {
            (8023, 2 | 8 | 13 | 14)
            | (8024, 2 | 3 | 6 | 8)
            | (8032, 2 | 3 | 8 | 13 | 14)
            | (8037, 2 | 3 | 8)
            | (8041, 2 | 3 | 8 | 13 | 14 | 15) => {}
            (8023, 15) => send(8, rescale(value, (160.0, 800.0, 0.623), SMALL)),
            (8023, 16) => send(2, rescale(value, (10.0, 90.0, 1.0), PERCENT)),
            (8023, 1) => send(1, rescale(value, (9.0, 25.0, 0.6), (4.0, 20.0, 0.6))),
            (8024, 0) => send(0, knee(value)),
            (8024 | 8037, 1) => {
                let size = size(value);
                if self.model_id == 8024 {
                    send(6, rescale(size, (10.0, 80.0, 1.0), (1.0, 200.0, 0.6)));
                    send(1, size.mul_add(8.0, 1.0));
                    send(3, size.mul_add(0.2, 0.5));
                } else {
                    send(1, size.mul_add(5.0, 3.0));
                    send(3, size.mul_add(0.2, 0.6));
                }
            }
            (8024, 13) => send(8, rescale(value, (50.0, 550.0, 1.0), SMALL)),
            (8024, 14) => send(2, value * 0.6),
            (8032, 15) => send(8, rescale(value, (100.0, 700.0, 0.6), SMALL)),
            (8032, 16) => send(2, rescale(value, (0.0, 70.0, 1.0), PERCENT)),
            (8032, 1) => {
                let curved = CURVE_8032.to_display(value).unwrap_or(0.0);
                send(1, to_normalized(curved, 4.0, 20.0, 0.6));
                send(3, display(value, 0.6, 0.8, 1.0));
            }
            (8037, 13) => send(8, rescale(value, (20.0, 500.0, 0.6), SMALL)),
            (8037, 14) => send(2, value * 0.7),
            (8041, 16) => send(8, rescale(value, (110.0, 800.0, 0.6), SMALL)),
            (8041, 17) => send(2, rescale(value, (10.0, 80.0, 1.0), PERCENT)),
            (8041, 1) => {
                send(1, rescale(value, (4.0, 9.0, 1.0), (4.0, 20.0, 0.6)));
                send(3, rescale(value, (45.0, 80.0, 1.0), PERCENT));
            }
            _ => return true,
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use tetrad_dsp_protocol::{DspCore, ModuleSlot};

    use super::super::super::{BlockBypass, SetupValue};
    use super::*;
    use crate::setup::drivers::media::test_media::TestMedia;
    use crate::setup::test_factory::{build, build_with};

    fn identity(model_id: u32) -> ModelIdentity {
        ModelIdentity {
            target: DeploymentTarget {
                core: DspCore::Core1,
                slot: ModuleSlot::try_from(6).unwrap(),
            },
            model_id,
            category: 8,
            fx_loop_send: None,
        }
    }

    fn values(raw: &[f32]) -> Vec<SetupValue> {
        raw.iter()
            .chain(&[0.0, 0.0])
            .map(|value| SetupValue::Float(value.to_bits()))
            .collect()
    }

    /// Constructor tables per model (jump table `0x55b068`).
    #[test]
    fn constructor_sends_both_const_tables() {
        let mut out = Vec::new();
        build(identity(8041), &mut out).unwrap();
        let buffers: Vec<_> = out
            .iter()
            .map(|write| match write {
                DspWrite::StockData {
                    buffer, payload, ..
                } => (*buffer, payload.segments()[0].va, payload.len()),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(buffers, [(0, 0x00a5_4ad8, 0x400), (1, 0x00a5_44d8, 0x180)]);
        assert_eq!(const_data(8008).unwrap()[1].len(), 0xc0);
    }

    /// Catalog path: display units with rounding for the step indices,
    /// pass-through for index 3, and the 8017 curve with its 0.0 tail.
    #[test]
    fn catalog_models_send_display_values() {
        let range = CatalogParameter {
            min: 1.0,
            max: 20.0,
            ..CatalogParameter::DEFAULT
        };
        let mut out = Vec::new();
        let mut block = build_with(
            identity(8017),
            &vec![range; 8],
            &TestMedia::default(),
            &mut out,
        )
        .unwrap();
        out.clear();
        block
            .push(
                &values(&[0.25, 0.5, 0.5, 0.5, 0.5, 0.5, 0.33]),
                BlockBypass::default(),
                None,
                &mut out,
            )
            .unwrap();
        let target = identity(8017).target;
        let shown = 0.25_f32.mul_add(19.0, 1.0);
        assert_eq!(
            out[1],
            DspWrite::parameter(target, 0, CURVE_8017.to_display(shown).unwrap_or(0.0))
        );
        assert_eq!(out[4], DspWrite::parameter(target, 3, 0.5));
        assert_eq!(
            out[7],
            DspWrite::parameter(target, 6, 0.33_f32.mul_add(19.0, 1.0).round())
        );
        assert_eq!(CURVE_8017.to_display(18.5), None);
        assert_eq!(CURVE_8017.to_display(f32::NAN), None);
    }

    /// Driver-local laws: 8024 SIZE fans out to 6, 1 and 3 with the stock
    /// fused multiply-adds; derived indices are never sent directly.
    #[test]
    fn size_control_fans_out_and_derived_indices_are_suppressed() {
        let mut out = Vec::new();
        let mut driver = Reverb::new(&identity(8024), &[], &mut out).unwrap();
        out.clear();
        assert!(!driver.update_parameter(1, 0.5, ParamSetMode::Push, &mut out));
        let size = 0.5_f32.mul_add(0.98, 0.02);
        let target = identity(8024).target;
        assert_eq!(
            out,
            [
                DspWrite::parameter(
                    target,
                    6,
                    to_normalized(size.mul_add(70.0, 10.0), 1.0, 200.0, 0.6)
                ),
                DspWrite::parameter(target, 1, size.mul_add(8.0, 1.0)),
                DspWrite::parameter(target, 3, size.mul_add(0.2, 0.5)),
            ]
        );
        out.clear();
        assert!(!driver.update_parameter(6, 0.5, ParamSetMode::Live, &mut out));
        assert!(driver.update_parameter(5, 0.5, ParamSetMode::Live, &mut out));
        assert!(out.is_empty());
    }

    /// Every registered payload (all drivers) resolves in the compact
    /// table built from the local pinned ZenUI.
    #[test]
    #[ignore = "reads $QC_CORPUS_DIR/rootfs/opt/neuraldsp/ZenUI"]
    fn stock_payloads_resolve_in_pinned_zenui() {
        use super::super::super::tables::{ElfTables, StockTables};
        let Some(path) = crate::corpus("rootfs/opt/neuraldsp/ZenUI") else {
            return;
        };
        let tables = ElfTables::drivers(std::fs::read(path).unwrap()).unwrap();
        for payload in super::super::stock_payloads() {
            assert_eq!(tables.read(payload).unwrap().len(), payload.len());
        }
    }
}
