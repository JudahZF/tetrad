//! Stock parameter conversion: normalized storage ⇄ display value, steps, defaults and display
//! text.
//!
//! Stored and DSP-facing parameter values are normalized. The display value is
//! `ParameterInfo::getPhysical` (`0x5cd110`); edits enter through
//! `setPhysical` (`0x5cd1fc`). Metadata parsing follows `XMLParser`
//! (`0x4e9ed8`) with the constructor defaults of `0x37d72c`.

mod curve;
mod display;
mod symbols;

pub use curve::{Breakpoint, Piecewise, PowerLaw};
pub use display::{qt_fuzzy_compare, qt_trimmed};
pub use symbols::{SYMBOLS, metadata_float, pugi_int, qt_float, symbol, xml_true};

/// Owned allocation cap; the catalog's largest piecewise table has nine points.
pub const MAX_BREAKPOINTS: usize = 256;
/// Owned cap on `stepNames` entries.
pub const MAX_STEP_NAMES: usize = 4096;
/// Steps and step size of a parameter without a `steps` attribute.
pub const ABSENT_STEPS: i32 = 101;
/// `lroundf(norm / max(stepSize, 1e-8))` floor for the divisor.
pub const MIN_STEP_DIVISOR: f32 = 1e-8;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConversionError {
    #[error("conversion metadata must be finite")]
    NonFiniteMetadata,
    #[error("a piecewise curve needs at least two breakpoints, found {0}")]
    TooFewBreakpoints(usize),
    #[error("{0} breakpoints exceed the owned limit")]
    TooManyBreakpoints(usize),
    #[error("{0} step names exceed the owned limit")]
    TooManyStepNames(usize),
    #[error("value lies outside every piecewise segment (stock yields 0.0)")]
    OutsideBreakpoints,
    #[error("value {0} has no defined integer conversion (stock lroundf/int overflow)")]
    IntegerRange(f32),
    #[error("step index {index} is outside 0..{steps}")]
    StepIndex { index: i64, steps: i32 },
    #[error("parameter type {0:?} has no text value")]
    TextValue(ParameterType),
}

pub type Result<T> = std::result::Result<T, ConversionError>;

/// The type map built by `_INIT_402`; unknown names map to `Float` because the
/// parser uses `std::map::operator[]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum ParameterType {
    #[default]
    Float = 0,
    Fader = 1,
    Int = 2,
    Switch = 3,
    RotarySwitch = 4,
    Meter = 5,
    StereoMeter = 6,
    GrMeter = 7,
    StereoGrMeter = 8,
    String = 9,
    ToggleButton = 10,
    ComboBox = 11,
    FloatWithLed = 12,
    Empty = 13,
}

impl ParameterType {
    pub fn from_xml(name: &str) -> Self {
        match name {
            "fader" => Self::Fader,
            "int" => Self::Int,
            "switch" => Self::Switch,
            "rotarySwitch" => Self::RotarySwitch,
            "meter" => Self::Meter,
            "stereoMeter" => Self::StereoMeter,
            "grMeter" => Self::GrMeter,
            "stereoGrMeter" => Self::StereoGrMeter,
            "string" => Self::String,
            "toggleButton" => Self::ToggleButton,
            "comboBox" => Self::ComboBox,
            "floatWithLed" => Self::FloatWithLed,
            "empty" => Self::Empty,
            _ => Self::Float,
        }
    }
}

/// A parameter default: normalized number, or text for `string` parameters.
#[derive(Debug, Clone, PartialEq)]
pub enum DefaultValue {
    Normalized(f32),
    Text(String),
}

/// Display strings. Empty means unset: stock tests `isEmpty()`, which treats
/// null and empty QStrings alike.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DisplayLabels {
    pub min: String,
    pub mid: String,
    pub max: String,
    /// Verbatim; `display_with_units` joins them to the value text with one space (`0x4c0ccc`).
    pub units: String,
    pub step_names: Vec<String>,
}

/// Raw `<Parameter>` attribute text; `None` means absent. Attributes are read
/// in a fixed parser order, so XML order does not matter.
#[derive(Debug, Clone, Copy, Default)]
pub struct XmlParameter<'a> {
    pub type_name: Option<&'a str>,
    pub steps: Option<&'a str>,
    pub skew: Option<&'a str>,
    pub min: Option<&'a str>,
    pub max: Option<&'a str>,
    pub default_value: Option<&'a str>,
    pub min_string: Option<&'a str>,
    pub mid_string: Option<&'a str>,
    pub max_string: Option<&'a str>,
    pub units: Option<&'a str>,
    pub step_names: Option<&'a str>,
    /// Effective `xValues`. Stock does not reset this per parameter: a model
    /// adapter must pass the previous parameter's text when it is absent.
    pub x_values: Option<&'a str>,
    pub y_values: Option<&'a str>,
    pub show_as_integer: Option<&'a str>,
    pub exp_assignable: Option<&'a str>,
    pub disabled: Option<&'a str>,
}

/// Conversion state of one `zenUI::ParameterInfo`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParameterScale {
    pub kind: ParameterType,
    pub law: PowerLaw,
    pub piecewise: Option<Piecewise>,
    steps: i32,
    step_size: f32,
    pub default: DefaultValue,
    pub labels: DisplayLabels,
    pub show_as_integer: bool,
    pub exp_assignable: bool,
    pub disabled: bool,
}

impl ParameterScale {
    /// Applies the stock attribute rules and defaults (min 0, max 1, skew 1,
    /// steps 101 with step size 0, `stepNames` `Off,On`, expAssignable true).
    pub fn from_xml(xml: &XmlParameter) -> Result<Self> {
        let kind = xml
            .type_name
            .map_or(ParameterType::Float, ParameterType::from_xml);
        let (steps, step_size) = match xml.steps {
            None => (ABSENT_STEPS, 0.0),
            Some(text) => {
                let steps = pugi_int(text);
                (steps, step_size_for(steps))
            }
        };
        let law = PowerLaw::new(
            xml.min.map_or(0.0, |t| metadata_float(t, 0.0)),
            xml.max.map_or(1.0, |t| metadata_float(t, 1.0)),
            xml.skew.map_or(1.0, |t| metadata_float(t, 1.0)),
        )?;
        let step_names: Vec<String> = match xml.step_names {
            None => vec!["Off".into(), "On".into()],
            Some(text) => text.split(',').map(str::to_owned).collect(),
        };
        if step_names.len() > MAX_STEP_NAMES {
            return Err(ConversionError::TooManyStepNames(step_names.len()));
        }
        let piecewise = match xml.y_values {
            None => None,
            Some(y) => breakpoints(xml.x_values.unwrap_or(""), y)?,
        };
        let default = match (xml.default_value, kind) {
            (None, _) => DefaultValue::Normalized(0.0),
            (Some(text), ParameterType::String) => DefaultValue::Text(text.to_owned()),
            (Some(text), _) => {
                // An unknown macro warns and falls back to min.
                let value = metadata_float(text, law.min);
                DefaultValue::Normalized(if piecewise.is_some() {
                    value
                } else {
                    law.to_normalized(value)
                })
            }
        };
        let text = |t: Option<&str>| t.unwrap_or_default().to_owned();
        Ok(Self {
            kind,
            law,
            piecewise,
            steps,
            step_size,
            default,
            labels: DisplayLabels {
                min: text(xml.min_string),
                mid: text(xml.mid_string),
                max: text(xml.max_string),
                units: text(xml.units),
                step_names,
            },
            show_as_integer: xml.show_as_integer.is_some_and(xml_true),
            exp_assignable: xml.exp_assignable.is_none_or(xml_true),
            disabled: xml.disabled.is_some_and(xml_true),
        })
    }

    pub fn steps(&self) -> i32 {
        self.steps
    }

    pub fn step_size(&self) -> f32 {
        self.step_size
    }

    /// `setSteps` (`0x5cc468`).
    pub fn set_steps(&mut self, steps: i32) {
        self.steps = steps;
        self.step_size = step_size_for(steps);
    }

    /// Normalized → display (`0x5cd110`).
    pub fn to_display(&self, normalized: f32) -> Result<f32> {
        match &self.piecewise {
            None => Ok(self.law.to_display(normalized)),
            Some(curve) => curve
                .to_display(normalized)
                .ok_or(ConversionError::OutsideBreakpoints),
        }
    }

    /// Display → normalized (`0x5cd1fc`). Never clamped to a step.
    pub fn to_normalized(&self, display: f32) -> Result<f32> {
        match &self.piecewise {
            None => Ok(self.law.to_normalized(display)),
            Some(curve) => curve
                .to_normalized(display)
                .ok_or(ConversionError::OutsideBreakpoints),
        }
    }

    /// `+0x8c`: `steps > 0 ? lroundf(norm / max(step, 1e-8)) : 0`.
    pub fn step_index(&self, normalized: f32) -> Result<i32> {
        if self.steps < 1 {
            return Ok(0);
        }
        lround(normalized / self.step_divisor())
    }

    /// `setStepIndex` (`0x5cc304`) asserts `0 <= index < steps`.
    pub fn step_value(&self, index: i64) -> Result<f32> {
        if !(0..i64::from(self.steps)).contains(&index) {
            return Err(ConversionError::StepIndex {
                index,
                steps: self.steps,
            });
        }
        Ok(self.step_size * index as f32)
    }

    /// Footswitch/button cycle (`0x5ccc08`): next step, wrapping to 0. A press
    /// below 0.5 or NaN leaves the value unchanged.
    pub fn next_step(&self, normalized: f32, press: f32) -> Result<f32> {
        if press.is_nan() || press < 0.5 || self.disabled {
            return Ok(normalized);
        }
        let mut index = if self.steps < 1 {
            1
        } else {
            i64::from(lround(normalized / self.step_divisor())?) + 1
        };
        if index >= i64::from(self.steps) {
            index = 0;
        }
        Ok(self.step_size * index as f32)
    }

    /// One step down or up, clamped to `0..steps` (`0x5ccc90`).
    pub fn nudge(&self, normalized: f32, direction: f32) -> Result<f32> {
        if self.disabled || self.steps < 1 || direction == 0.0 || direction.is_nan() {
            return Ok(normalized);
        }
        let current = i64::from(lround(normalized / self.step_divisor())?);
        let index = if direction < 0.0 {
            (current - 1).max(0)
        } else {
            (current + 1).min(i64::from(self.steps) - 1)
        };
        Ok(self.step_size * index as f32)
    }

    /// Encoder increment (`0x5cca94`); separate f32 operations, `fmod` in f64.
    pub fn encoder(&self, normalized: f32, ticks: i32) -> f32 {
        let step = if self.steps < 2 {
            0.01
        } else {
            1.0 / (self.steps - 1) as f32
        };
        let mut delta = step * ticks as f32;
        let mut value = if ticks.unsigned_abs() == 1 || self.kind == ParameterType::RotarySwitch {
            delta + normalized
        } else {
            if self.kind != ParameterType::ComboBox {
                let factor = (0.01 / f64::from(step)) as f32;
                delta *= if factor <= 1.0 {
                    factor
                } else {
                    factor.round()
                };
            }
            normalized + delta
        };
        if self.kind != ParameterType::RotarySwitch {
            let rest = (f64::from(value) % f64::from(step)) as f32;
            if f64::from((rest - step).abs()) > 0.0001 {
                value = if rest > 0.5 {
                    value + (step - rest)
                } else {
                    value - rest
                };
            }
        }
        value.clamp(0.0, 1.0)
    }

    fn step_divisor(&self) -> f32 {
        if self.step_size > MIN_STEP_DIVISOR {
            self.step_size
        } else {
            MIN_STEP_DIVISOR
        }
    }
}

fn step_size_for(steps: i32) -> f32 {
    if steps < 2 {
        1.0
    } else {
        1.0 / (steps - 1) as f32
    }
}

/// `lroundf` into a 32-bit `long`; NaN and overflow are refused (owned).
fn lround(value: f32) -> Result<i32> {
    let rounded = value.round();
    if rounded.is_nan() || rounded < i32::MIN as f32 || rounded >= 2_147_483_648.0 {
        return Err(ConversionError::IntegerRange(value));
    }
    Ok(rounded as i32)
}

/// `xValues`/`yValues` pairing: split on commas, mismatched counts give no
/// curve, unparsable points are skipped.
fn breakpoints(x: &str, y: &str) -> Result<Option<Piecewise>> {
    let (xs, ys): (Vec<&str>, Vec<&str>) = (x.split(',').collect(), y.split(',').collect());
    if xs.len() != ys.len() {
        return Ok(None);
    }
    if xs.len() > MAX_BREAKPOINTS {
        return Err(ConversionError::TooManyBreakpoints(xs.len()));
    }
    let points: Vec<Breakpoint> = xs
        .iter()
        .zip(&ys)
        .filter_map(|(x, y)| {
            Some(Breakpoint {
                x: qt_float(x)?,
                y: qt_float(y)?,
            })
        })
        .collect();
    if points.is_empty() {
        return Ok(None);
    }
    Piecewise::new(points).map(Some)
}

#[cfg(test)]
mod tests;
