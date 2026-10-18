use std::fmt;

pub const MAX_ROWS: usize = crate::GRID_ROWS;
pub const MAX_ROW_MODELS: usize = 8;
pub const MAX_PARAMETERS: usize = 256;
pub const MAX_MODELS: usize = 256;
pub const MAX_PARAMETERS_TOTAL: usize = 16_384;
pub const MAX_SCENE_VALUES: usize = 131_072;
pub const MAX_ITEMS: usize = 1_024;
pub const MAX_UTF16_UNITS: usize = 65_536;
pub const MAX_BYTES: usize = 65_536;
pub const MAX_TOTAL_UNITS: usize = 1_048_576;
pub const MAX_SCALARS: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    Field(&'static str),
    Index(usize),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Path(Vec<PathSegment>);

impl Path {
    pub(crate) fn root(field: &'static str) -> Self {
        Self(vec![PathSegment::Field(field)])
    }
    pub(crate) fn field(&self, field: &'static str) -> Self {
        let mut next = self.clone();
        next.0.push(PathSegment::Field(field));
        next
    }
    pub(crate) fn index(&self, index: usize) -> Self {
        let mut next = self.clone();
        next.0.push(PathSegment::Index(index));
        next
    }
    pub fn segments(&self) -> &[PathSegment] {
        &self.0
    }
}

impl fmt::Display for Path {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, segment) in self.0.iter().enumerate() {
            match segment {
                PathSegment::Field(field) => {
                    if i > 0 {
                        write!(formatter, ".")?;
                    }
                    write!(formatter, "{field}")?;
                }
                PathSegment::Index(index) => write!(formatter, "[{index}]")?,
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionReason {
    Limit { maximum: usize, actual: usize },
    SceneBanks { expected: usize, actual: usize },
    BankExceedsDescriptors { values: usize, descriptors: usize },
    DuplicateMapKey(u32),
    SignalingNaN,
    InvalidDefaultScene(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionError {
    pub path: Path,
    pub reason: AdmissionReason,
}
impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?}", self.path, self.reason)
    }
}
impl std::error::Error for AdmissionError {}

/// Literal QString content; null and nonnull empty remain distinct in storage.
/// Unpaired surrogates and embedded NUL are admitted. No normalization occurs.
#[derive(Debug, Clone, Default)]
pub struct Utf16 {
    units: Vec<u16>,
    is_null: bool,
}
impl Utf16 {
    pub fn from_units(units: Vec<u16>) -> Result<Self, AdmissionError> {
        check_limit(&Path::root("utf16"), units.len(), MAX_UTF16_UNITS)?;
        Ok(Self {
            units,
            is_null: false,
        })
    }
    pub fn from_literal(text: &str) -> Result<Self, AdmissionError> {
        Self::from_units(text.encode_utf16().take(MAX_UTF16_UNITS + 1).collect())
    }
    pub fn null() -> Self {
        Self {
            units: Vec::new(),
            is_null: true,
        }
    }
    pub fn units(&self) -> &[u16] {
        &self.units
    }
    pub fn is_null(&self) -> bool {
        self.is_null
    }
}

pub(crate) fn check_limit(
    path: &Path,
    actual: usize,
    maximum: usize,
) -> Result<(), AdmissionError> {
    if actual > maximum {
        Err(AdmissionError {
            path: path.clone(),
            reason: AdmissionReason::Limit { maximum, actual },
        })
    } else {
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct Budget {
    models: usize,
    parameters: usize,
    scene_values: usize,
    units: usize,
    scalars: usize,
}
impl Budget {
    fn charge(
        counter: &mut usize,
        amount: usize,
        maximum: usize,
        path: &Path,
    ) -> Result<(), AdmissionError> {
        let actual = counter.checked_add(amount).ok_or_else(|| AdmissionError {
            path: path.clone(),
            reason: AdmissionReason::Limit {
                maximum,
                actual: usize::MAX,
            },
        })?;
        check_limit(path, actual, maximum)?;
        *counter = actual;
        Ok(())
    }
    pub(crate) fn models(&mut self, path: &Path) -> Result<(), AdmissionError> {
        Self::charge(&mut self.models, 1, MAX_MODELS, path)
    }
    pub(crate) fn parameters(&mut self, path: &Path) -> Result<(), AdmissionError> {
        Self::charge(&mut self.parameters, 1, MAX_PARAMETERS_TOTAL, path)
    }
    pub(crate) fn scene_values(&mut self, n: usize, path: &Path) -> Result<(), AdmissionError> {
        Self::charge(&mut self.scene_values, n, MAX_SCENE_VALUES, path)
    }
    pub(crate) fn units(&mut self, n: usize, path: &Path) -> Result<(), AdmissionError> {
        Self::charge(&mut self.units, n, MAX_TOTAL_UNITS, path)
    }
    pub(crate) fn scalars(&mut self, n: usize, path: &Path) -> Result<(), AdmissionError> {
        Self::charge(&mut self.scalars, n, MAX_SCALARS, path)
    }
    pub(crate) fn text(&mut self, text: &Utf16, path: &Path) -> Result<(), AdmissionError> {
        self.units(text.units.len(), path)
    }
    pub(crate) fn bytes(&mut self, bytes: &[u8], path: &Path) -> Result<(), AdmissionError> {
        check_limit(path, bytes.len(), MAX_BYTES)?;
        self.units(bytes.len(), path)
    }
    pub(crate) fn vector(&mut self, n: usize, path: &Path) -> Result<(), AdmissionError> {
        check_limit(path, n, MAX_ITEMS)?;
        self.scalars(n, path)
    }
}
