//! Parametric4Driver (4008): constructor 0x56a188, `+0x30` 0x56a210. The four
//! FREQ values (2, 4, 6, 8) go out through the block's own `ParameterInfo`
//! (`0x2e5f58` copy): `to_normalized(toDisplay(v), min, max, skew)`, with
//! `toDisplay` the breakpoint law (0x5cd110) and `0x64b8f8` mode 1. Other
//! indices pass.

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{CatalogParameter, DspWrite, ModelIdentity, ParamSetMode, SetupError};
use super::catalog::to_normalized;
use super::{Driver, DriverKind};

const FREQUENCIES: [u32; 4] = [2, 4, 6, 8];

#[derive(Debug)]
pub(super) struct Parametric4 {
    target: DeploymentTarget,
    frequencies: [CatalogParameter; 4],
}

impl Parametric4 {
    /// No DSP writes. Refused when the catalog lacks a FREQ parameter.
    pub(super) fn new(
        identity: &ModelIdentity,
        catalog: &[CatalogParameter],
    ) -> Result<Self, SetupError> {
        let info = |index: u32| {
            catalog
                .get(index as usize)
                .cloned()
                .ok_or(SetupError::MissingCatalog {
                    model_id: identity.model_id,
                    index,
                })
        };
        Ok(Self {
            target: identity.target,
            frequencies: [info(2)?, info(4)?, info(6)?, info(8)?],
        })
    }
}

impl Driver for Parametric4 {
    fn kind(&self) -> DriverKind {
        DriverKind::Parametric4
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let Some(at) = FREQUENCIES.iter().position(|&frequency| frequency == index) else {
            return true;
        };
        let info = &self.frequencies[at];
        let value = to_normalized(info.display(value), info.min, info.max, info.skew);
        out.push(DspWrite::parameter(self.target, index, value));
        false
    }
}

#[cfg(test)]
mod tests {
    use tetrad_core::conversion::{Breakpoint, Piecewise};

    use super::*;
    use crate::setup::drivers::amps::tests::identity;

    /// A FREQ with breakpoints maps through them and back over min..max; a
    /// value past the last point is 0.0 in stock, so min goes out. Without
    /// breakpoints the power law round-trips. GAIN passes.
    #[test]
    fn frequencies_use_breakpoints_then_the_power_law() {
        let points = [(0.0, 20.0), (0.5, 60.0), (1.0, 200.0)]
            .map(|(x, y)| Breakpoint { x, y })
            .to_vec();
        let lo = CatalogParameter {
            min: 20.0,
            max: 200.0,
            piecewise: Some(Piecewise::new(points).unwrap()),
            ..CatalogParameter::DEFAULT
        };
        let plain = CatalogParameter {
            min: 100.0,
            max: 1000.0,
            ..CatalogParameter::DEFAULT
        };
        let mut catalog = vec![CatalogParameter::DEFAULT; 10];
        catalog[2] = lo;
        catalog[4] = plain;
        let mut driver = Parametric4::new(&identity(4008), &catalog).unwrap();
        let target = identity(4008).target;
        let mut out = Vec::new();
        assert!(!driver.update_parameter(2, 0.75, ParamSetMode::Live, &mut out));
        assert!(!driver.update_parameter(2, 2.0, ParamSetMode::Live, &mut out));
        assert!(!driver.update_parameter(4, 0.25, ParamSetMode::Live, &mut out));
        assert!(driver.update_parameter(3, 0.25, ParamSetMode::Live, &mut out));
        assert_eq!(
            out,
            [
                DspWrite::parameter(target, 2, (130.0 - 20.0) / 180.0),
                DspWrite::parameter(target, 2, 0.0),
                DspWrite::parameter(target, 4, 0.25),
            ]
        );
    }
}
