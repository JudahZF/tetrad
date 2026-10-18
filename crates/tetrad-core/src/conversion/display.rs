//! Parameter value text: `ParameterInfo::getDisplayValue` (`0x5cd904`), number
//! formatter `0x5ccdc0`, pan text `0x5cd41c` and the type branches of
//! `ParameterControl::onValueChanged` (`0x4c8948`).

use super::{ConversionError, ParameterScale, ParameterType, Result, lround};

/// Qt 4.8 float `qFuzzyCompare`: `|a−b| <= 0.00001·min(|a|,|b|)` in f32. A zero
/// operand therefore needs exact equality.
pub fn qt_fuzzy_compare(a: f32, b: f32) -> bool {
    (a - b).abs() <= 0.00001 * a.abs().min(b.abs())
}

/// `QString::trimmed` with the pinned Qt 4.8 `QChar::isSpace` set.
pub fn qt_trimmed(text: &str) -> &str {
    text.trim_matches(|c: char| {
        matches!(
            c,
            '\u{9}'..='\u{d}'
                | ' '
                | '\u{a0}'
                | '\u{1680}'
                | '\u{180e}'
                | '\u{2000}'..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
        )
    })
}

/// Combo box entries carry a hidden prefix before U+00A4 (`0xa4a80c`).
const COMBO_SEPARATOR: char = '\u{a4}';

impl ParameterScale {
    /// Pan flag `+0x104`: labels exactly `L`, `C`, `R`.
    pub fn is_pan(&self) -> bool {
        self.labels.min == "L" && self.labels.mid == "C" && self.labels.max == "R"
    }

    /// Value text for the widget, or `TextValue` for types that draw no text
    /// (switch, string, meters). Units are not appended here.
    pub fn display_text(&self, normalized: f32) -> Result<String> {
        match self.kind {
            ParameterType::RotarySwitch | ParameterType::ComboBox => {
                let name = self.step_name(normalized)?;
                Ok(if self.kind == ParameterType::ComboBox {
                    let last = name.rsplit(COMBO_SEPARATOR).next().unwrap_or_default();
                    qt_trimmed(last).to_owned()
                } else {
                    name.to_owned()
                })
            }
            ParameterType::Switch
            | ParameterType::String
            | ParameterType::Meter
            | ParameterType::StereoMeter
            | ParameterType::GrMeter
            | ParameterType::StereoGrMeter
            | ParameterType::FloatWithLed => Err(ConversionError::TextValue(self.kind)),
            ParameterType::Float
            | ParameterType::Fader
            | ParameterType::Int
            | ParameterType::ToggleButton
            | ParameterType::Empty => self.value_text(normalized),
        }
    }

    /// `0x5cd904`: pan text, then min/max/mid labels, then the number.
    pub fn value_text(&self, normalized: f32) -> Result<String> {
        if self.is_pan() {
            return self.pan_text(normalized);
        }
        let display = self.to_display(normalized)?;
        let labels = &self.labels;
        if qt_fuzzy_compare(self.law.min, display) && !labels.min.is_empty() {
            return Ok(labels.min.clone());
        }
        if qt_fuzzy_compare(self.law.max, display) && !labels.max.is_empty() {
            return Ok(labels.max.clone());
        }
        // The midpoint always uses the power law, even for piecewise curves.
        if !labels.mid.is_empty() && qt_fuzzy_compare(self.law.to_display(0.5), display) {
            return Ok(labels.mid.clone());
        }
        self.number_text(display)
    }

    /// Number formatter `0x5ccdc0`. Thousands use a truncated `k` form
    /// (`12500` → `12k5`); no sign is added and units are not converted.
    pub fn number_text(&self, value: f32) -> Result<String> {
        if (value.abs() - 10_000.0).abs() < 0.001 {
            return Ok(if value > 0.0 { "10k" } else { "-10k" }.into());
        }
        if value >= 10_000.0 {
            if value >= 2_147_483_648.0 {
                return Err(ConversionError::IntegerRange(value));
            }
            let whole = value as i32;
            return Ok(format!(
                "{}k{}",
                (value / 1000.0) as i32,
                whole % 1000 / 100
            ));
        }
        let per_step = (self.law.max - self.law.min) / self.steps().wrapping_sub(1) as f32;
        if per_step >= 1.0 || self.show_as_integer {
            return Ok(lround(value)?.to_string());
        }
        let shown = if -0.0001 < value && value < 0.0 {
            -value
        } else {
            value
        };
        let precision = if f64::from(per_step) >= 0.1 { 1 } else { 2 };
        Ok(format!("{:.*}", precision, f64::from(shown)))
    }

    /// `0x5cd41c`: `p = norm·100 − 50` (exact in f64, then f32); centre within
    /// 0.5, else the rounded magnitude, a space, then the side label.
    fn pan_text(&self, normalized: f32) -> Result<String> {
        let p = f64::from(normalized).mul_add(100.0, -50.0) as f32;
        if p.abs() < 0.5 {
            return Ok(self.labels.mid.clone());
        }
        if p.is_nan() {
            return Err(ConversionError::IntegerRange(p));
        }
        Ok(if p >= 0.0 {
            format!("{} {}", lround(p)?, self.labels.max)
        } else {
            format!("{} {}", lround(-p)?, self.labels.min)
        })
    }

    /// `0x4c0ccc`: the value text, then a space and the units when units are set.
    pub fn display_with_units(&self, normalized: f32) -> Result<String> {
        let text = self.display_text(normalized)?;
        if self.labels.units.is_empty() {
            return Ok(text);
        }
        Ok(format!("{text} {}", self.labels.units))
    }

    /// Rotary/combo text index; stock asserts `stepNames.size() == steps`.
    fn step_name(&self, normalized: f32) -> Result<&str> {
        let index = self.step_index(normalized)?;
        usize::try_from(index)
            .ok()
            .filter(|_| self.labels.step_names.len() == self.steps() as usize)
            .and_then(|i| self.labels.step_names.get(i))
            .map(String::as_str)
            .ok_or(ConversionError::StepIndex {
                index: i64::from(index),
                steps: self.steps(),
            })
    }
}
