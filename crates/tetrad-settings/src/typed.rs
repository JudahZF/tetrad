//! Shared pieces of the typed model: validation errors, load issues, the
//! stock-faithful key reader and bounded number types.

use thiserror::Error;

use crate::{FormatIssue, Hardware, SettingsFile, SettingsFileId, Variant, VariantError};

/// A rejected setting value.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum SettingError {
    #[error("{name} {value} is outside {min}..={max}")]
    OutOfRange {
        name: &'static str,
        value: f64,
        min: f64,
        max: f64,
    },
    #[error("{name} has no stock value {value}")]
    UnknownValue { name: &'static str, value: String },
}

/// Why a stored value or line was not used. Load continues with the stock fallback.
#[derive(Clone, Debug, PartialEq)]
pub enum LoadIssueKind {
    /// The file has a malformed line. Qt skips it and drops it on the next save.
    Format(FormatIssue),
    /// The `@Variant` or `@` text form did not decode.
    Undecodable(VariantError),
    /// Qt's conversion failed (`ok == false`); stock code then gets 0 or false.
    NotConvertible,
    /// Converted, but outside the stock range or enum. Holds the stored value.
    Invalid(SettingError),
}

/// `key` is `None` for [`LoadIssueKind::Format`], which is about a line, not a key.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadIssue {
    pub file: SettingsFileId,
    pub key: Option<String>,
    pub kind: LoadIssueKind,
}

/// Reads keys like `QSettings::value(key, default).toX()`: a missing key
/// gives the default, a present key goes through Qt's conversion. Every
/// problem is recorded. Issues from [`KeyReader::check`] keep the rejected
/// value, so only non-secret keys may use `check`.
pub(crate) struct KeyReader<'a> {
    pub file: &'a SettingsFile,
    pub id: SettingsFileId,
    pub hardware: Hardware,
    pub issues: &'a mut Vec<LoadIssue>,
}

impl KeyReader<'_> {
    pub fn issue(&mut self, key: &str, kind: LoadIssueKind) {
        self.issues.push(LoadIssue {
            file: self.id,
            key: Some(key.to_owned()),
            kind,
        });
    }

    /// The decoded value, or `None` when absent or undecodable.
    pub fn variant(&mut self, key: &str) -> Option<Variant> {
        match self.file.value(key)? {
            Ok(v) => Some(v),
            Err(e) => {
                self.issue(key, LoadIssueKind::Undecodable(e));
                None
            }
        }
    }

    pub fn bool(&mut self, key: &str, default: bool) -> bool {
        self.variant(key).map_or(default, |v| v.to_bool())
    }

    /// `toInt()`: a present but unconvertible value reads as 0, as in stock.
    pub fn int(&mut self, key: &str, default: i32) -> i32 {
        match self.variant(key) {
            None => default,
            Some(v) => v.to_int().unwrap_or_else(|| {
                self.issue(key, LoadIssueKind::NotConvertible);
                0
            }),
        }
    }

    /// `toFloat()`: a present but unconvertible value reads as 0.0.
    pub fn f32(&mut self, key: &str, default: f32) -> f32 {
        match self.variant(key) {
            None => default,
            Some(v) => v.to_f32().unwrap_or_else(|| {
                self.issue(key, LoadIssueKind::NotConvertible);
                0.0
            }),
        }
    }

    pub fn string(&mut self, key: &str, default: &str) -> String {
        match self.variant(key) {
            None => default.to_owned(),
            Some(v) => v.to_string_value().unwrap_or_else(|| {
                self.issue(key, LoadIssueKind::NotConvertible);
                String::new()
            }),
        }
    }

    /// `QList<int>` from a `QList<int>` user value or a list of numbers.
    pub fn int_list(&mut self, key: &str) -> Vec<i32> {
        match self.variant(key) {
            None => Vec::new(),
            Some(v) => v.to_int_list().unwrap_or_else(|| {
                self.issue(key, LoadIssueKind::NotConvertible);
                Vec::new()
            }),
        }
    }

    /// Validates a converted value; on error records it and uses `fallback`.
    pub fn check<T>(&mut self, key: &str, result: Result<T, SettingError>, fallback: T) -> T {
        result.unwrap_or_else(|e| {
            self.issue(key, LoadIssueKind::Invalid(e));
            fallback
        })
    }
}

/// Writes a field only when it changed, so keys the user never touched are
/// not added to the file.
pub(crate) struct KeyWriter<'a> {
    pub file: &'a mut SettingsFile,
}

impl KeyWriter<'_> {
    pub fn put<T: PartialEq>(
        &mut self,
        key: &str,
        new: &T,
        old: &T,
        encode: impl FnOnce(&T) -> Variant,
    ) {
        if new != old {
            self.file.set(key, &encode(new));
        }
    }

    pub fn bool(&mut self, key: &str, new: bool, old: bool) {
        self.put(key, &new, &old, |v| Variant::Bool(*v));
    }

    pub fn int(&mut self, key: &str, new: i32, old: i32) {
        self.put(key, &new, &old, |v| Variant::Int(*v));
    }

    /// Stock stores these through `QVariant(float)`, i.e. `@Variant`.
    pub fn f32(&mut self, key: &str, new: f32, old: f32) {
        self.put(key, &new, &old, |v| Variant::Float(*v));
    }

    pub fn string(&mut self, key: &str, new: &str, old: &str) {
        self.put(key, &new, &old, |v| Variant::String((*v).to_owned()));
    }
}

/// An integer setting with a stock range and default.
macro_rules! bounded {
    ($(#[$meta:meta])* $name:ident: $t:ty, $min:expr, $max:expr, default $default:expr) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name($t);

        impl $name {
            pub const MIN: $t = $min;
            pub const MAX: $t = $max;
            pub const DEFAULT: Self = Self($default);

            pub fn new(value: $t) -> Result<Self, $crate::SettingError> {
                if (Self::MIN..=Self::MAX).contains(&value) {
                    Ok(Self(value))
                } else {
                    Err($crate::SettingError::OutOfRange {
                        name: stringify!($name),
                        value: value as f64,
                        min: Self::MIN as f64,
                        max: Self::MAX as f64,
                    })
                }
            }

            /// Clamps into range, as stock sliders do.
            pub fn clamped(value: $t) -> Self {
                Self(value.clamp(Self::MIN, Self::MAX))
            }

            pub const fn get(self) -> $t {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::DEFAULT
            }
        }
    };
}
pub(crate) use bounded;

/// A closed set of stock values stored as an integer.
macro_rules! stock_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $value:expr),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub const fn to_stock(self) -> i32 {
                match self {
                    $(Self::$variant => $value),+
                }
            }

            pub fn from_stock(value: i32) -> Result<Self, $crate::SettingError> {
                match value {
                    $(v if v == $value => Ok(Self::$variant),)+
                    other => Err($crate::SettingError::UnknownValue {
                        name: stringify!($name),
                        value: other.to_string(),
                    }),
                }
            }
        }
    };
}
pub(crate) use stock_enum;
