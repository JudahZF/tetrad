use crate::{Error, Result};

/// Mathematical finite power-law mapping, not a universal parameter law or
/// stock bit parity. Stock forward VA 0x64b9e4 uses logf/expf; this API uses
/// powf. The inverse preclamp is an owned overflow policy. ARM/libm comparison
/// and exceptional stock metadata behavior remain unresolved.
#[derive(Debug, Clone, Copy)]
pub struct PowerMapping {
    min: f32,
    max: f32,
    exponent: f32,
}

impl PowerMapping {
    pub fn new(min: f32, max: f32, exponent: f32) -> Result<Self> {
        if !min.is_finite()
            || !max.is_finite()
            || !exponent.is_finite()
            || min >= max
            || exponent <= 0.0
            || !(max - min).is_finite()
            || !exponent.recip().is_finite()
        {
            return Err(Error::InvalidMapping);
        }
        Ok(Self { min, max, exponent })
    }

    pub fn to_physical(self, normalized: f32) -> Result<f32> {
        if !normalized.is_finite() {
            return Err(Error::NonFinite);
        }
        // Pinned ARM mapper VA 0x64b958 uses VFMA.F32, not two rounded operations.
        Ok(normalized
            .clamp(0.0, 1.0)
            .powf(1.0 / self.exponent)
            .mul_add(self.max - self.min, self.min))
    }

    pub fn to_normalized(self, physical: f32) -> Result<f32> {
        if !physical.is_finite() {
            return Err(Error::NonFinite);
        }
        // Clamp before subtraction to avoid overflow for extreme finite inputs.
        Ok(
            ((physical.clamp(self.min, self.max) - self.min) / (self.max - self.min))
                .powf(self.exponent),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_conversion_clamps_and_uses_reciprocal_forward_exponent() {
        let mapping = PowerMapping::new(-10.0, 10.0, 2.0).unwrap();
        assert_eq!(mapping.to_physical(0.25).unwrap(), 0.0);
        assert_eq!(mapping.to_normalized(0.0).unwrap(), 0.25);
        assert_eq!(mapping.to_physical(-1.0).unwrap(), -10.0);
        assert_eq!(mapping.to_physical(2.0).unwrap(), 10.0);
        assert_eq!(mapping.to_normalized(f32::MIN).unwrap(), 0.0);
        assert_eq!(mapping.to_normalized(f32::MAX).unwrap(), 1.0);
        for i in 0..=100 {
            let normalized = i as f32 / 100.0;
            let restored = mapping
                .to_normalized(mapping.to_physical(normalized).unwrap())
                .unwrap();
            assert!((restored - normalized).abs() < 1e-6);
        }
    }

    #[test]
    fn forward_mapper_preserves_the_recovered_fused_multiply_add() {
        let epsilon = f32::EPSILON;
        let mapping = PowerMapping::new(-1.0, epsilon, 1.0).unwrap();
        let normalized = 1.0 - epsilon;
        let separate = normalized * (1.0 + epsilon) - 1.0;
        assert_eq!(separate, 0.0);
        assert_eq!(mapping.to_physical(normalized).unwrap(), -epsilon * epsilon);
    }

    #[test]
    fn invalid_and_exceptional_metadata_are_rejected_instead_of_guessed() {
        for (min, max, exponent) in [
            (1.0, 1.0, 1.0),
            (2.0, 1.0, 1.0),
            (0.0, 1.0, 0.0),
            (0.0, 1.0, -1.0),
            (0.0, 1.0, f32::NAN),
            (f32::MIN, f32::MAX, 1.0),
        ] {
            assert!(PowerMapping::new(min, max, exponent).is_err());
        }
        let mapping = PowerMapping::new(0.0, 1.0, 1.0).unwrap();
        assert_eq!(mapping.to_normalized(f32::INFINITY), Err(Error::NonFinite));
        assert_eq!(mapping.to_physical(f32::NAN), Err(Error::NonFinite));
    }
}
