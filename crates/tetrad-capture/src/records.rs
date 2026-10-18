//! Bounded inspection of a caller-delimited prefix of recovered capture records.
//!
//! This is not a serialized graph reader or runtime node registry. The caller
//! supplies the record count and first runtime ordinal; neither is inferred from
//! a selector, payload word, link record or end of input. Tags5/6 remain opaque.
//! Other operators, links, transforms and graph execution are unsupported.

use crate::affine;
use std::fmt;

pub const MAX_RECORDS: usize = 1_024;
pub const MAX_PREFIX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_records: usize,
    pub max_prefix_bytes: usize,
    pub affine: affine::Limits,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_records: MAX_RECORDS,
            max_prefix_bytes: MAX_PREFIX_BYTES,
            affine: affine::Limits::default(),
        }
    }
}

#[derive(Debug)]
pub enum RecordKind<'a> {
    Affine(affine::AffineRecord<'a>),
    Opaque5(&'a [u8; 32]),
    Opaque6(&'a [u8; 32]),
}

#[derive(Debug)]
pub struct Record<'a> {
    pub ordinal: u32,
    pub offset: usize,
    pub kind: RecordKind<'a>,
}

#[derive(Debug)]
pub struct RecordPrefix<'a> {
    pub records: Vec<Record<'a>>,
    pub consumed: usize,
    pub remainder: &'a [u8],
    /// None means the last consumed record used u32::MAX; continuation must refuse.
    pub next_ordinal: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    RecordLimit,
    PrefixByteLimit,
    OrdinalOverflow,
    Truncated {
        ordinal: u32,
        offset: usize,
    },
    UnsupportedTag {
        ordinal: u32,
        offset: usize,
        tag: u32,
    },
    Affine {
        ordinal: u32,
        offset: usize,
        source: crate::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "capture record prefix: {self:?}")
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Affine { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Parse exactly count records, preserving their order and caller-based ordinals.
///
/// No partial prefix is returned on failure. Input remains borrowed and unchanged.
/// Bounds are owned resource policies, not recovered stock graph limits. The
/// prefix byte limit covers consumed records, not the untouched remainder.
/// A larger caller limit cannot raise either hard cap. Affine parsing preserves
/// tensor bits but does not invoke its numerical reference or graph execution.
pub fn parse_records_prefix(
    bytes: &[u8],
    count: usize,
    first_ordinal: u32,
    limits: Limits,
) -> Result<RecordPrefix<'_>, Error> {
    if count > limits.max_records || count > MAX_RECORDS {
        return Err(Error::RecordLimit);
    }
    let count32 = u32::try_from(count).map_err(|_| Error::OrdinalOverflow)?;
    if count32 > 0 && first_ordinal.checked_add(count32 - 1).is_none() {
        return Err(Error::OrdinalOverflow);
    }
    let max_bytes = limits.max_prefix_bytes.min(MAX_PREFIX_BYTES);
    // Every admitted layout consumes at least32 bytes. Check before allocation.
    if count
        .checked_mul(32)
        .is_none_or(|minimum| minimum > max_bytes)
    {
        return Err(Error::PrefixByteLimit);
    }
    let mut records = Vec::with_capacity(count);
    let mut offset = 0_usize;
    for index in 0..count32 {
        // Preflight above proves ordinal addition fits for each reached record.
        let ordinal = first_ordinal + index;
        let tail = bytes
            .get(offset..)
            .ok_or(Error::Truncated { ordinal, offset })?;
        let tag_bytes = tail.get(..4).ok_or(Error::Truncated { ordinal, offset })?;
        let tag = u32::from_le_bytes(tag_bytes.try_into().expect("checked four-byte word"));
        let (kind, consumed) =
            match tag {
                4 => {
                    let affine_limits = affine::Limits {
                        max_record_bytes: limits.affine.max_record_bytes.min(max_bytes - offset),
                        ..limits.affine
                    };
                    let (record, consumed) = affine::AffineRecord::parse(tail, affine_limits)
                        .map_err(|source| Error::Affine {
                            ordinal,
                            offset,
                            source,
                        })?;
                    (RecordKind::Affine(record), consumed)
                }
                5 | 6 => {
                    if max_bytes - offset < 32 {
                        return Err(Error::PrefixByteLimit);
                    }
                    let record: &[u8; 32] = tail
                        .get(..32)
                        .ok_or(Error::Truncated { ordinal, offset })?
                        .try_into()
                        .expect("checked32-byte record");
                    let kind = if tag == 5 {
                        RecordKind::Opaque5(record)
                    } else {
                        RecordKind::Opaque6(record)
                    };
                    (kind, 32)
                }
                tag => {
                    return Err(Error::UnsupportedTag {
                        ordinal,
                        offset,
                        tag,
                    });
                }
            };
        let next = offset.checked_add(consumed).ok_or(Error::PrefixByteLimit)?;
        if next > max_bytes {
            return Err(Error::PrefixByteLimit);
        }
        records.push(Record {
            ordinal,
            offset,
            kind,
        });
        offset = next;
    }
    Ok(RecordPrefix {
        records,
        consumed: offset,
        remainder: &bytes[offset..],
        next_ordinal: first_ordinal.checked_add(count32),
    })
}
