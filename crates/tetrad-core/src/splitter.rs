use crate::{Error, ParameterValue, Result, RuntimeParameter};

/// Logical TYPE selectors, not their encoded runtime parameter values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitterType {
    Type0,
    Type1,
    Type2,
}

/// Caller-resolved encoding through model 10004's TYPE metadata. Stock uses
/// metadata conversion at VA 0x6e4170 and stores the resulting float in all
/// eight scenes. No raw integer/normalized selector assumption is made here.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedSplitterType {
    pub selector: SplitterType,
    pub encoded_value: f32,
}

/// A parameter-only conversion. Graph split/mix records, ports and legality
/// remain external; this plan does not claim a complete model/graph importer.
#[derive(Debug, Clone, Copy)]
pub struct LegacySplitterPlan {
    selector: SplitterType,
    mappings: &'static [(usize, usize)],
}

impl LegacySplitterPlan {
    pub const TARGET_MODEL_ID: u32 = 10004;

    pub fn new(source_model_id: u32) -> Result<Self> {
        let (selector, mappings): (_, &'static [(usize, usize)]) = match source_model_id {
            10000 => (SplitterType::Type1, &[(0, 3), (1, 4), (2, 1)]),
            10001 => (SplitterType::Type2, &[(0, 5), (1, 6), (2, 1)]),
            10002 => (SplitterType::Type0, &[(0, 2), (1, 1)]),
            _ => return Err(Error::UnsupportedSplitter(source_model_id)),
        };
        Ok(Self { selector, mappings })
    }

    pub fn selector(self) -> SplitterType {
        self.selector
    }

    pub fn mappings(self) -> &'static [(usize, usize)] {
        self.mappings
    }

    /// Source runtime parameters must be initialized by the earlier import
    /// stages. Copies scenes and controls together. Unmapped destinations keep
    /// the explicit target defaults. TYPE encoding is required from the caller.
    /// Bounds and encoding are checked before producing any output.
    pub fn apply(
        self,
        source: &[RuntimeParameter],
        target_defaults: &[RuntimeParameter],
        type_encoding: ResolvedSplitterType,
    ) -> Result<Vec<RuntimeParameter>> {
        if type_encoding.selector != self.selector {
            return Err(Error::SplitterTypeEncoding {
                expected: self.selector,
                actual: type_encoding.selector,
            });
        }
        for &(from, to) in self.mappings {
            if from >= source.len() {
                return Err(Error::SourceParameterPosition {
                    position: from,
                    count: source.len(),
                });
            }
            if to >= target_defaults.len() {
                return Err(Error::ParameterPosition {
                    position: to,
                    count: target_defaults.len(),
                });
            }
        }
        let mut target = target_defaults.to_vec();
        target[0].scenes =
            std::array::from_fn(|_| ParameterValue::Float(type_encoding.encoded_value));
        for &(from, to) in self.mappings {
            target[to] = source[from].clone();
        }
        Ok(target)
    }
}
