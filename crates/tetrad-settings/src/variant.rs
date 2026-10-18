//! The `QVariant` subset the stock settings files store, its INI text form
//! (`QSettingsPrivate::variantToString` / `stringToVariant`, Qt 4.8) and the
//! `QDataStream` `Qt_4_0` encoding inside `@Variant(..)`.
//!
//! `QSettings` writes `@Variant` with stream version `Qt_4_0`: big-endian,
//! no null flag after the type id, user types as id 127 plus a NUL-ended
//! type name, `float` as 4 bytes. Stock files hold `float` (id 135),
//! `QVariantMap` (8) and the user type `QList<int>`.

use thiserror::Error;

use crate::ini::IniValue;

/// Qt 4 type ids (`QVariant::Type`, `QMetaType::Type`).
mod type_id {
    pub const INVALID: u32 = 0;
    pub const BOOL: u32 = 1;
    pub const INT: u32 = 2;
    pub const UINT: u32 = 3;
    pub const LONG_LONG: u32 = 4;
    pub const ULONG_LONG: u32 = 5;
    pub const DOUBLE: u32 = 6;
    pub const MAP: u32 = 8;
    pub const LIST: u32 = 9;
    pub const STRING: u32 = 10;
    pub const STRING_LIST: u32 = 11;
    pub const BYTE_ARRAY: u32 = 12;
    pub const DATE_TIME: u32 = 16;
    pub const USER: u32 = 127;
    pub const FLOAT: u32 = 135;
}

const INT_LIST_NAME: &[u8] = b"QList<int>";
const NULL_LEN: u32 = 0xFFFF_FFFF;

/// A Qt 4 `QDateTime` as streamed: Julian day, milliseconds of the day
/// (`u32::MAX` when the time is null) and the `Qt::TimeSpec` byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QtDateTime {
    pub julian_day: u32,
    pub msecs_of_day: u32,
    pub spec: i8,
}

/// A decoded settings value.
///
/// Plain INI text decodes to [`Variant::String`]; Qt converts it on access
/// (`toInt`, `toBool`, ...), and so do the `to_*` methods here.
#[derive(Clone, Debug, PartialEq)]
pub enum Variant {
    Invalid,
    Bool(bool),
    Int(i32),
    UInt(u32),
    LongLong(i64),
    ULongLong(u64),
    Double(f64),
    Float(f32),
    String(String),
    StringList(Vec<String>),
    ByteArray(Vec<u8>),
    List(Vec<Variant>),
    /// `QVariantMap` in stream order. Qt 4 streams a `QMap` from the last
    /// key to the first; build new maps with [`Variant::map`].
    Map(Vec<(String, Variant)>),
    /// User type `QList<int>`.
    IntList(Vec<i32>),
    DateTime(QtDateTime),
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum VariantError {
    #[error("@Variant payload ends early")]
    Truncated,
    #[error("@Variant type id {0} is not supported")]
    UnsupportedType(u32),
    #[error("@Variant user type {0:?} is not supported")]
    UnsupportedUserType(String),
    #[error("@Variant string has an odd byte length")]
    OddStringLength,
    #[error("@Variant nesting is too deep")]
    TooDeep,
    /// `@Rect(`, `@Size(` or `@Point(`; no stock setting uses them.
    #[error("INI text form {0}..) is not supported")]
    UnsupportedTextForm(&'static str),
}

impl Variant {
    /// A `QVariantMap` in Qt 4 stream order (keys descending).
    pub fn map(entries: impl IntoIterator<Item = (String, Variant)>) -> Self {
        let mut entries: Vec<_> = entries.into_iter().collect();
        entries.sort_by(|a, b| b.0.encode_utf16().cmp(a.0.encode_utf16()));
        Variant::Map(entries)
    }

    /// `stringToVariant`, plus `stringListToVariantList` for lists.
    pub fn from_ini(value: &IniValue) -> Result<Self, VariantError> {
        match value {
            IniValue::Text(text) => Self::from_text(text),
            IniValue::List(items) => {
                let single_at = items
                    .iter()
                    .any(|s| s.starts_with('@') && !s.starts_with("@@"));
                if single_at {
                    items
                        .iter()
                        .map(|s| Self::from_text(s))
                        .collect::<Result<_, _>>()
                        .map(Variant::List)
                } else {
                    Ok(Variant::StringList(
                        items
                            .iter()
                            .map(|s| {
                                s.strip_prefix('@')
                                    .filter(|r| r.starts_with('@'))
                                    .unwrap_or(s)
                                    .to_owned()
                            })
                            .collect(),
                    ))
                }
            }
        }
    }

    fn from_text(text: &str) -> Result<Self, VariantError> {
        if !text.starts_with('@') {
            return Ok(Variant::String(text.to_owned()));
        }
        if let Some(rest) = text.strip_prefix("@@") {
            return Ok(Variant::String(format!("@{rest}")));
        }
        if text.ends_with(')') {
            if let Some(body) = text.strip_prefix("@ByteArray(") {
                return Ok(Variant::ByteArray(latin1(&body[..body.len() - 1])));
            }
            if let Some(body) = text.strip_prefix("@Variant(") {
                // Qt streams from the bytes after "@Variant(", ')' included.
                return Reader::new(&latin1(body)).variant();
            }
            if text == "@Invalid()" {
                return Ok(Variant::Invalid);
            }
            if let Some(form) = ["@Rect(", "@Size(", "@Point("]
                .into_iter()
                .find(|p| text.starts_with(p))
            {
                return Err(VariantError::UnsupportedTextForm(form));
            }
        }
        Ok(Variant::String(text.to_owned()))
    }

    /// `variantToString`, or a string list for `QStringList` and
    /// `QVariantList` values without exactly one item (as `writeIniFile`).
    pub fn to_ini(&self) -> IniValue {
        match self {
            // One item writes the same bytes as plain text and reads back as
            // text, so keep it as text in memory too.
            Variant::StringList(items) if items.len() == 1 => {
                IniValue::Text(escape_at(items[0].clone()))
            }
            Variant::StringList(items) => {
                IniValue::List(items.iter().map(|s| escape_at(s.clone())).collect())
            }
            Variant::List(items) if items.len() != 1 => {
                IniValue::List(items.iter().map(Variant::to_text).collect())
            }
            other => IniValue::Text(other.to_text()),
        }
    }

    fn to_text(&self) -> String {
        match self {
            Variant::Invalid => "@Invalid()".to_owned(),
            Variant::ByteArray(bytes) => {
                format!(
                    "@ByteArray({})",
                    bytes.iter().map(|&b| char::from(b)).collect::<String>()
                )
            }
            Variant::String(s) => escape_at(s.clone()),
            Variant::Bool(b) => b.to_string(),
            Variant::Int(v) => v.to_string(),
            Variant::UInt(v) => v.to_string(),
            Variant::LongLong(v) => v.to_string(),
            Variant::ULongLong(v) => v.to_string(),
            Variant::Double(v) => escape_at(format_g(*v, 15)),
            Variant::Float(_)
            | Variant::StringList(_)
            | Variant::List(_)
            | Variant::Map(_)
            | Variant::IntList(_)
            | Variant::DateTime(_) => {
                let mut w = Vec::new();
                write_variant(self, &mut w);
                let mut text = String::from("@Variant(");
                text.extend(w.iter().map(|&b| char::from(b)));
                text.push(')');
                text
            }
        }
    }

    /// `QVariant::toBool`.
    pub fn to_bool(&self) -> bool {
        match self {
            Variant::Bool(b) => *b,
            Variant::Int(v) => *v != 0,
            Variant::UInt(v) => *v != 0,
            Variant::LongLong(v) => *v != 0,
            Variant::ULongLong(v) => *v != 0,
            Variant::Double(v) => *v != 0.0,
            Variant::Float(v) => *v != 0.0,
            Variant::String(s) => {
                let s = s.to_lowercase();
                !(s.is_empty() || s == "0" || s == "false")
            }
            Variant::ByteArray(b) => {
                let b = b.to_ascii_lowercase();
                !(b.is_empty() || b == b"0" || b == b"false")
            }
            _ => false,
        }
    }

    /// `QVariant::toInt` with its `ok` flag: `None` where Qt returns 0 and
    /// `ok == false`.
    pub fn to_int(&self) -> Option<i32> {
        match self {
            Variant::Bool(b) => Some(i32::from(*b)),
            Variant::Int(v) => Some(*v),
            Variant::UInt(v) => Some(*v as i32),
            Variant::LongLong(v) => Some(*v as i32),
            Variant::ULongLong(v) => Some(*v as i32),
            Variant::Double(v) => Some(q_round64(*v) as i32),
            Variant::Float(v) => Some(q_round64(f64::from(*v)) as i32),
            Variant::String(s) => s.trim().parse::<i64>().ok().map(|v| v as i32),
            _ => None,
        }
    }

    /// `QVariant::toFloat` / `toDouble` with the `ok` flag.
    pub fn to_f64(&self) -> Option<f64> {
        match self {
            Variant::Bool(b) => Some(f64::from(u8::from(*b))),
            Variant::Int(v) => Some(f64::from(*v)),
            Variant::UInt(v) => Some(f64::from(*v)),
            Variant::LongLong(v) => Some(*v as f64),
            Variant::ULongLong(v) => Some(*v as f64),
            Variant::Double(v) => Some(*v),
            Variant::Float(v) => Some(f64::from(*v)),
            Variant::String(s) => s.trim().parse::<f64>().ok().filter(|v| v.is_finite()),
            _ => None,
        }
    }

    pub fn to_f32(&self) -> Option<f32> {
        match self {
            Variant::Float(v) => Some(*v),
            other => other.to_f64().map(|v| v as f32),
        }
    }

    /// `QVariant::toString` for the types settings use.
    pub fn to_string_value(&self) -> Option<String> {
        match self {
            Variant::String(s) => Some(s.clone()),
            Variant::Bool(_)
            | Variant::Int(_)
            | Variant::UInt(_)
            | Variant::LongLong(_)
            | Variant::ULongLong(_) => Some(self.to_text()),
            Variant::Double(v) => Some(format_g(*v, 15)),
            Variant::ByteArray(b) => Some(b.iter().map(|&c| char::from(c)).collect()),
            _ => None,
        }
    }

    /// `QVariant::toStringList`.
    pub fn to_string_list(&self) -> Vec<String> {
        match self {
            Variant::StringList(items) => items.clone(),
            Variant::List(items) => items.iter().filter_map(Variant::to_string_value).collect(),
            Variant::Invalid => Vec::new(),
            other => other.to_string_value().into_iter().collect(),
        }
    }

    /// Integers from a `QList<int>` user value or from `toList()`, which
    /// gives nothing for plain text.
    pub fn to_int_list(&self) -> Option<Vec<i32>> {
        match self {
            Variant::IntList(v) => Some(v.clone()),
            Variant::List(items) => items.iter().map(Variant::to_int).collect(),
            Variant::StringList(items) => items.iter().map(|s| s.trim().parse().ok()).collect(),
            Variant::Invalid => Some(Vec::new()),
            _ => None,
        }
    }
}

/// `variantToString` doubles a leading `@` of plain strings.
fn escape_at(s: String) -> String {
    if s.starts_with('@') {
        format!("@{s}")
    } else {
        s
    }
}

fn latin1(s: &str) -> Vec<u8> {
    // QString::toLatin1 maps units above 0xFF to '?'.
    s.chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

/// Qt 4 `qRound64`.
fn q_round64(d: f64) -> i64 {
    (d + 0.5).floor() as i64
}

/// `QString::number(v, 'g', precision)`: shortest of fixed and exponent
/// form, trailing zeros removed, exponent with sign and at least 2 digits.
fn format_g(v: f64, precision: usize) -> String {
    if v == 0.0 {
        return "0".to_owned();
    }
    if !v.is_finite() {
        return if v.is_nan() {
            "nan"
        } else if v > 0.0 {
            "inf"
        } else {
            "-inf"
        }
        .to_owned();
    }
    let sci = format!("{:.*e}", precision - 1, v);
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if exp < -4 || exp >= precision as i32 {
        let mantissa = trim_zeros(mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    } else {
        let decimals = (precision as i32 - 1 - exp).max(0) as usize;
        trim_zeros(&format!("{v:.decimals$}")).to_owned()
    }
}

fn trim_zeros(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

/// Deepest `QVariantList` / `QVariantMap` nesting the reader accepts.
const MAX_NESTING: usize = 32;

struct Reader<'a> {
    data: &'a [u8],
    depth: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, depth: 0 }
    }

    /// A child value one level deeper. Bounded so hostile input cannot overflow the stack.
    fn nested(&mut self) -> Result<Variant, VariantError> {
        if self.depth == MAX_NESTING {
            return Err(VariantError::TooDeep);
        }
        self.depth += 1;
        let value = self.variant();
        self.depth -= 1;
        value
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], VariantError> {
        let (head, rest) = self
            .data
            .split_first_chunk::<N>()
            .ok_or(VariantError::Truncated)?;
        self.data = rest;
        Ok(*head)
    }

    fn u32(&mut self) -> Result<u32, VariantError> {
        self.take().map(u32::from_be_bytes)
    }

    fn bytes(&mut self, len: usize) -> Result<&'a [u8], VariantError> {
        if self.data.len() < len {
            return Err(VariantError::Truncated);
        }
        let (head, rest) = self.data.split_at(len);
        self.data = rest;
        Ok(head)
    }

    /// `QByteArray`; a null array reads as empty.
    fn byte_array(&mut self) -> Result<Vec<u8>, VariantError> {
        match self.u32()? {
            NULL_LEN => Ok(Vec::new()),
            len => Ok(self.bytes(len as usize)?.to_vec()),
        }
    }

    /// `QString` (UTF-16BE); a null string reads as empty.
    fn string(&mut self) -> Result<String, VariantError> {
        let len = self.u32()?;
        if len == NULL_LEN {
            return Ok(String::new());
        }
        if len % 2 != 0 {
            return Err(VariantError::OddStringLength);
        }
        let units: Vec<u16> = self
            .bytes(len as usize)?
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        Ok(String::from_utf16_lossy(&units))
    }

    fn count(&mut self) -> Result<usize, VariantError> {
        let n = self.u32()? as usize;
        // Every element takes at least one byte; reject impossible counts early.
        if n > self.data.len() {
            return Err(VariantError::Truncated);
        }
        Ok(n)
    }

    fn variant(&mut self) -> Result<Variant, VariantError> {
        use type_id::*;
        let id = self.u32()?;
        Ok(match id {
            INVALID => {
                self.string()?;
                Variant::Invalid
            }
            BOOL => Variant::Bool(self.take::<1>()?[0] != 0),
            INT => Variant::Int(self.take().map(i32::from_be_bytes)?),
            UINT => Variant::UInt(self.u32()?),
            LONG_LONG => Variant::LongLong(self.take().map(i64::from_be_bytes)?),
            ULONG_LONG => Variant::ULongLong(self.take().map(u64::from_be_bytes)?),
            DOUBLE => Variant::Double(self.take().map(f64::from_be_bytes)?),
            FLOAT => Variant::Float(self.take().map(f32::from_be_bytes)?),
            STRING => Variant::String(self.string()?),
            BYTE_ARRAY => Variant::ByteArray(self.byte_array()?),
            STRING_LIST => {
                let n = self.count()?;
                Variant::StringList((0..n).map(|_| self.string()).collect::<Result<_, _>>()?)
            }
            LIST => {
                let n = self.count()?;
                Variant::List((0..n).map(|_| self.nested()).collect::<Result<_, _>>()?)
            }
            MAP => {
                let n = self.count()?;
                Variant::Map(
                    (0..n)
                        .map(|_| Ok((self.string()?, self.nested()?)))
                        .collect::<Result<_, VariantError>>()?,
                )
            }
            DATE_TIME => Variant::DateTime(QtDateTime {
                julian_day: self.u32()?,
                msecs_of_day: self.u32()?,
                spec: self.take::<1>()?[0] as i8,
            }),
            USER => {
                let mut name = self.byte_array()?;
                if name.last() == Some(&0) {
                    name.pop();
                }
                if name != INT_LIST_NAME {
                    return Err(VariantError::UnsupportedUserType(
                        String::from_utf8_lossy(&name).into_owned(),
                    ));
                }
                let n = self.count()?;
                Variant::IntList(
                    (0..n)
                        .map(|_| self.take().map(i32::from_be_bytes))
                        .collect::<Result<_, _>>()?,
                )
            }
            other => return Err(VariantError::UnsupportedType(other)),
        })
    }
}

fn write_string(s: &str, out: &mut Vec<u8>) {
    let units: Vec<u16> = s.encode_utf16().collect();
    out.extend_from_slice(&((units.len() * 2) as u32).to_be_bytes());
    for unit in units {
        out.extend_from_slice(&unit.to_be_bytes());
    }
}

fn write_variant(v: &Variant, out: &mut Vec<u8>) {
    use type_id::*;
    let put = |out: &mut Vec<u8>, id: u32| out.extend_from_slice(&id.to_be_bytes());
    match v {
        Variant::Invalid => {
            put(out, INVALID);
            out.extend_from_slice(&NULL_LEN.to_be_bytes());
        }
        Variant::Bool(b) => {
            put(out, BOOL);
            out.push(u8::from(*b));
        }
        Variant::Int(x) => {
            put(out, INT);
            out.extend_from_slice(&x.to_be_bytes());
        }
        Variant::UInt(x) => {
            put(out, UINT);
            out.extend_from_slice(&x.to_be_bytes());
        }
        Variant::LongLong(x) => {
            put(out, LONG_LONG);
            out.extend_from_slice(&x.to_be_bytes());
        }
        Variant::ULongLong(x) => {
            put(out, ULONG_LONG);
            out.extend_from_slice(&x.to_be_bytes());
        }
        Variant::Double(x) => {
            put(out, DOUBLE);
            out.extend_from_slice(&x.to_be_bytes());
        }
        Variant::Float(x) => {
            put(out, FLOAT);
            out.extend_from_slice(&x.to_be_bytes());
        }
        Variant::String(s) => {
            put(out, STRING);
            write_string(s, out);
        }
        Variant::ByteArray(b) => {
            put(out, BYTE_ARRAY);
            out.extend_from_slice(&(b.len() as u32).to_be_bytes());
            out.extend_from_slice(b);
        }
        Variant::StringList(items) => {
            put(out, STRING_LIST);
            out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            items.iter().for_each(|s| write_string(s, out));
        }
        Variant::List(items) => {
            put(out, LIST);
            out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            items.iter().for_each(|item| write_variant(item, out));
        }
        Variant::Map(entries) => {
            put(out, MAP);
            out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
            for (key, value) in entries {
                write_string(key, out);
                write_variant(value, out);
            }
        }
        Variant::IntList(items) => {
            put(out, USER);
            // `operator<<(const char *)`: length with the NUL, then the NUL-ended name.
            out.extend_from_slice(&((INT_LIST_NAME.len() + 1) as u32).to_be_bytes());
            out.extend_from_slice(INT_LIST_NAME);
            out.push(0);
            out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            items
                .iter()
                .for_each(|x| out.extend_from_slice(&x.to_be_bytes()));
        }
        Variant::DateTime(dt) => {
            put(out, DATE_TIME);
            out.extend_from_slice(&dt.julian_day.to_be_bytes());
            out.extend_from_slice(&dt.msecs_of_day.to_be_bytes());
            out.push(dt.spec as u8);
        }
    }
}
