//! Stock normalized/display value laws.
//!
//! Operation order follows the ARM code, including the fused final multiply-add
//! (`VFMA.F32` at `0x64b958`, `0x64b9f4`, `0x5cd1c4`, `0x5cd2a8`). Host `ln`, `exp`
//! and `powf` are references; glibc ARM bit parity is not proved.

use super::{ConversionError, Result};

/// The power law at `0x64b8f8`. Stock does not validate its metadata; this type
/// only requires finite fields so that every conversion stays defined.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerLaw {
    pub min: f32,
    pub max: f32,
    pub skew: f32,
}

impl PowerLaw {
    pub fn new(min: f32, max: f32, skew: f32) -> Result<Self> {
        if !(min.is_finite() && max.is_finite() && skew.is_finite()) {
            return Err(ConversionError::NonFiniteMetadata);
        }
        Ok(Self { min, max, skew })
    }

    /// Mode 0: clamp to `0..=1` (NaN passes), then `min + u^(1/skew)·(max−min)`.
    /// Skew 1 and `u <= 0` take the linear branch without `logf`.
    pub fn to_display(self, normalized: f32) -> f32 {
        // `clamp` keeps NaN, like the stock branches.
        let u = normalized.clamp(0.0, 1.0);
        let shaped = if self.skew == 1.0 || u <= 0.0 {
            u
        } else {
            (u.ln() / self.skew).exp()
        };
        shaped.mul_add(self.max - self.min, self.min)
    }

    /// Mode 1: `((v−min)/(max−min))^skew` after clamping. Unordered compares skip
    /// both clamps, so a NaN ratio (including a zero-width range at its minimum)
    /// passes through.
    pub fn to_normalized(self, display: f32) -> f32 {
        let ratio = (display - self.min) / (self.max - self.min);
        // `clamp` returns NaN for a NaN ratio, as the unordered stock compares do.
        let clamped = ratio.clamp(0.0, 1.0);
        if self.skew == 1.0 {
            clamped
        } else {
            clamped.powf(self.skew)
        }
    }
}

/// One `(normalized x, display y)` breakpoint from `xValues`/`yValues`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Breakpoint {
    pub x: f32,
    pub y: f32,
}

/// The piecewise law at `0x5cd110`/`0x5cd1fc`. Points stay in stored order;
/// stock never sorts or validates them.
#[derive(Debug, Clone, PartialEq)]
pub struct Piecewise(Vec<Breakpoint>);

impl Piecewise {
    /// Stock returns 0.0 for fewer than two points; that state is refused here so
    /// a scale never silently maps every value to zero.
    pub fn new(points: Vec<Breakpoint>) -> Result<Self> {
        if points.len() < 2 {
            return Err(ConversionError::TooFewBreakpoints(points.len()));
        }
        if points.len() > super::MAX_BREAKPOINTS {
            return Err(ConversionError::TooManyBreakpoints(points.len()));
        }
        if points.iter().any(|p| !(p.x.is_finite() && p.y.is_finite())) {
            return Err(ConversionError::NonFiniteMetadata);
        }
        Ok(Self(points))
    }

    pub fn points(&self) -> &[Breakpoint] {
        &self.0
    }

    /// The first segment also extrapolates below its start. A value above every
    /// later `x` has no segment and yields `None` (stock returns 0.0). NaN also
    /// yields `None`: the stock loop never takes its exit branch and runs off the end.
    pub fn to_display(&self, normalized: f32) -> Option<f32> {
        if normalized.is_nan() {
            return None;
        }
        let p = &self.0;
        let mut end = 1;
        if normalized > p[1].x {
            loop {
                end += 1;
                if end == p.len() {
                    return None;
                }
                if p[end].x >= normalized {
                    break;
                }
            }
        }
        let (a, b) = (p[end - 1], p[end]);
        Some(((b.y - a.y) / (b.x - a.x)).mul_add(normalized - a.x, a.y))
    }

    /// The first segment whose `y` interval contains the value, in either
    /// direction. No extrapolation: `None` where stock returns 0.0.
    pub fn to_normalized(&self, display: f32) -> Option<f32> {
        self.0.windows(2).find_map(|w| {
            let (a, b) = (w[0], w[1]);
            let inside = (a.y <= display && display <= b.y) || (display <= a.y && b.y <= display);
            inside.then(|| ((b.x - a.x) / (b.y - a.y)).mul_add(display - a.y, a.x))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(pairs: &[(f32, f32)]) -> Piecewise {
        Piecewise::new(pairs.iter().map(|&(x, y)| Breakpoint { x, y }).collect()).unwrap()
    }

    #[test]
    fn power_law_follows_stock_branches_and_nan_order() {
        let law = PowerLaw::new(20.0, 20_000.0, 0.3).unwrap();
        assert_eq!(law.to_display(-0.5), 20.0);
        assert_eq!(law.to_display(2.0), 20_000.0);
        assert!(law.to_display(f32::NAN).is_nan());
        let flat = PowerLaw::new(3.0, 3.0, 2.0).unwrap();
        assert_eq!(flat.to_display(0.7), 3.0);
        assert_eq!(flat.to_normalized(2.0), 0.0);
        for i in 0..=20 {
            let u = i as f32 / 20.0;
            let back = law.to_normalized(law.to_display(u));
            assert!((back - u).abs() < 1e-4, "{u} -> {back}");
        }
    }

    #[test]
    fn power_inverse_nan_passes_through_both_branches() {
        let curved = PowerLaw::new(20.0, 20_000.0, 0.3).unwrap();
        let linear = PowerLaw::new(20.0, 20_000.0, 1.0).unwrap();
        assert!(curved.to_normalized(f32::NAN).is_nan());
        assert!(linear.to_normalized(f32::NAN).is_nan());
        let flat = PowerLaw::new(3.0, 3.0, 2.0).unwrap();
        assert!(flat.to_normalized(3.0).is_nan());
    }

    #[test]
    fn piecewise_nan_has_no_segment() {
        let law = points(&[(0.0, 20.0), (0.5, 60.0), (1.0, 200.0)]);
        assert_eq!(law.to_display(f32::NAN), None);
    }

    #[test]
    fn linear_power_branch_keeps_the_fused_multiply_add() {
        let law = PowerLaw::new(-1.0, f32::EPSILON, 1.0).unwrap();
        let u = 1.0 - f32::EPSILON;
        assert_eq!(u * (1.0 + f32::EPSILON) - 1.0, 0.0);
        assert_eq!(law.to_display(u), -f32::EPSILON * f32::EPSILON);
    }

    #[test]
    fn piecewise_extrapolates_only_below_and_inverts_inside_segments() {
        let law = points(&[(0.0, 20.0), (0.5, 60.0), (1.0, 200.0)]);
        assert_eq!(law.to_display(0.25), Some(40.0));
        assert_eq!(law.to_display(0.75), Some(130.0));
        assert_eq!(law.to_display(-0.5), Some(-20.0));
        assert_eq!(law.to_display(1.5), None);
        assert_eq!(law.to_normalized(130.0), Some(0.75));
        assert_eq!(law.to_normalized(10.0), None);
        // Descending segments invert too.
        let falling = points(&[(0.0, 10.0), (1.0, 0.0)]);
        assert_eq!(falling.to_normalized(2.5), Some(0.75));
        assert!(Piecewise::new(vec![Breakpoint { x: 0.0, y: 0.0 }]).is_err());
    }
}
