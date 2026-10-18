//! Catalog input for drivers that convert values on the ARM, and the stock
//! power law those conversions use (VA `0x64b8f8`,
//! `tetrad_core::conversion::PowerLaw`).

use tetrad_core::conversion::{Piecewise, PowerLaw};

/// The catalog `ParameterInfo` fields drivers read for one parameter of the
/// block's model (ModelRepo values after symbol resolution).
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogParameter {
    /// `+0x60` (VA `0x5cc5c0`).
    pub min: f32,
    /// `+0x64` (VA `0x5cc634`).
    pub max: f32,
    /// `+0x98` (VA `0x5cca4c`).
    pub skew: f32,
    /// `+0x54`.
    pub steps: i32,
    /// `+0x90`: `steps < 2 ? 1 : 1/(steps − 1)`, 0.0 without `steps`.
    pub step: f32,
    /// `+0x108..+0x10c`: the `xValues`/`yValues` breakpoints, if any.
    pub piecewise: Option<Piecewise>,
}

impl CatalogParameter {
    /// Stock default for a parameter without metadata: min 0, max 1, skew 1,
    /// 101 steps of size 0.0, no breakpoints.
    pub const DEFAULT: Self = Self {
        min: 0.0,
        max: 1.0,
        skew: 1.0,
        steps: 101,
        step: 0.0,
        piecewise: None,
    };

    /// Step index (VA `0x5cc994`): `steps > 0 ? lroundf(v / max(step,
    /// 1e-8)) : 0`. Rust saturates where `lroundf` overflows (owned).
    pub(super) fn step_index(&self, value: f32) -> i32 {
        if self.steps > 0 {
            (value / self.step.max(1e-8)).round() as i32
        } else {
            0
        }
    }

    /// Normalized → display (VA `0x5cd110`): the breakpoints, else the power
    /// law. Past the last breakpoint and for NaN stock returns 0.0.
    pub(super) fn display(&self, value: f32) -> f32 {
        match &self.piecewise {
            Some(curve) => curve.to_display(value).unwrap_or(0.0),
            None => to_display(value, self.min, self.max, self.skew),
        }
    }
}

/// Stock power law, normalized → display (`0x64b8f8` mode 0): clamp to 0..1
/// (NaN passes), `exp(ln(u)/skew)` unless skew is 1 or `u ≤ 0`, then a fused
/// `min + u·(max − min)`.
pub(super) fn to_display(value: f32, min: f32, max: f32, skew: f32) -> f32 {
    PowerLaw { min, max, skew }.to_display(value)
}

/// Stock power law, display → normalized (`0x64b8f8` mode 1):
/// `(v − min)/(max − min)` clamped to 0..1 (NaN passes), then `powf(·, skew)`
/// unless skew is 1.
pub(super) fn to_normalized(value: f32, min: f32, max: f32, skew: f32) -> f32 {
    PowerLaw { min, max, skew }.to_normalized(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ARM semantics of `0x64b8f8`: NaN passes the clamps, skew 1 is linear,
    /// the inverse clamps before the power.
    #[test]
    fn power_law_matches_stock_branches() {
        assert_eq!(to_display(2.0, 4.0, 20.0, 0.6), 20.0);
        assert_eq!(to_display(-1.0, 4.0, 20.0, 0.6), 4.0);
        assert_eq!(to_display(0.5, 0.0, 10.0, 1.0), 5.0);
        assert_eq!(
            to_display(0.25, 9.0, 25.0, 0.6),
            ((0.25_f32).ln() / 0.6).exp().mul_add(16.0, 9.0)
        );
        assert!(to_display(f32::NAN, 0.0, 1.0, 0.5).is_nan());
        assert_eq!(to_normalized(30.0, 4.0, 20.0, 0.6), 1.0);
        assert_eq!(to_normalized(12.0, 4.0, 20.0, 1.0), 0.5);
        assert_eq!(to_normalized(12.0, 4.0, 20.0, 0.6), 0.5_f32.powf(0.6));
    }
}
