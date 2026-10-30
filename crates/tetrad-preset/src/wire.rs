use crate::{Error, Limits, Result, schema};

#[derive(Debug, Clone)]
pub(crate) struct Message {
    pub fields: Vec<Field>,
    pub schema: Option<&'static schema::MessageSchema>,
}

#[derive(Debug, Clone)]
pub(crate) struct Field {
    pub number: u32,
    pub tag: Raw,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub(crate) enum Value {
    Varint { raw: Raw, value: u64 },
    Fixed64([u8; 8]),
    Bytes { length: Raw, data: Vec<u8> },
    Message { length: Raw, body: Message },
    Group { body: Message, end: Raw },
    Fixed32([u8; 4]),
}

/// The bytes of one varint as stored, overlong forms included. [`read_varint`] reads at most
/// ten bytes, so they stay inline: a preset has thousands of tags, values and length prefixes.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Raw {
    len: u8,
    bytes: [u8; MAX_VARINT],
}

const MAX_VARINT: usize = 10;

impl Raw {
    /// Panics on more than ten bytes, which no varint has.
    pub fn new(bytes: &[u8]) -> Self {
        let mut raw = Self {
            len: bytes.len() as u8,
            bytes: [0; MAX_VARINT],
        };
        raw.bytes[..bytes.len()].copy_from_slice(bytes);
        raw
    }
}

impl std::ops::Deref for Raw {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

impl std::fmt::Debug for Raw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (**self).fmt(f)
    }
}

impl Value {
    pub fn wire(&self) -> u8 {
        match self {
            Self::Varint { .. } => 0,
            Self::Fixed64(_) => 1,
            Self::Bytes { .. } | Self::Message { .. } => 2,
            Self::Group { .. } => 3,
            Self::Fixed32(_) => 5,
        }
    }

    pub fn size(&self) -> usize {
        match self {
            Self::Varint { raw, .. } => raw.len(),
            Self::Fixed64(_) => 8,
            Self::Fixed32(_) => 4,
            Self::Bytes { length, data } => length.len() + data.len(),
            Self::Message { length, body } => length.len() + body.size(),
            Self::Group { body, end } => body.size() + end.len(),
        }
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        match self {
            Self::Varint { raw, .. } => out.extend_from_slice(raw),
            Self::Fixed64(bytes) => out.extend_from_slice(bytes),
            Self::Fixed32(bytes) => out.extend_from_slice(bytes),
            Self::Bytes { length, data } => {
                out.extend_from_slice(length);
                out.extend_from_slice(data);
            }
            Self::Message { length, body } => {
                out.extend_from_slice(length);
                body.write(out);
            }
            Self::Group { body, end } => {
                body.write(out);
                out.extend_from_slice(end);
            }
        }
    }
}

impl Message {
    pub fn size(&self) -> usize {
        self.fields
            .iter()
            .map(|f| f.tag.len() + f.value.size())
            .sum()
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        for field in &self.fields {
            out.extend_from_slice(&field.tag);
            field.value.write(out);
        }
    }

    pub fn positions(&self, number: u32) -> impl Iterator<Item = usize> + '_ {
        self.fields
            .iter()
            .enumerate()
            .filter_map(move |(i, f)| (f.number == number).then_some(i))
    }

    /// Replaces one leaf. Only changed ancestor payload sizes get new length prefixes.
    pub fn replace(&mut self, path: &[usize], field: Field) {
        if path.len() == 1 {
            if path[0] == self.fields.len() {
                self.fields.push(field);
            } else {
                self.fields[path[0]] = field;
            }
        } else if let Value::Message { length, body } = &mut self.fields[path[0]].value {
            let old_size = body.size();
            body.replace(&path[1..], field);
            let new_size = body.size();
            if new_size != old_size {
                *length = encode_varint(new_size as u64);
            }
        } else {
            unreachable!("edit path checked before mutation")
        }
    }
}

pub(crate) fn encode_varint(mut value: u64) -> Raw {
    let mut raw = Raw::default();
    while value > 127 {
        raw.bytes[usize::from(raw.len)] = (value as u8 & 127) | 128;
        raw.len += 1;
        value >>= 7;
    }
    raw.bytes[usize::from(raw.len)] = value as u8;
    raw.len += 1;
    raw
}

pub(crate) fn read_varint(data: &[u8], pos: &mut usize, base: usize) -> Result<u64> {
    let start = *pos;
    let mut value = 0_u64;
    for shift in (0..70).step_by(7) {
        let byte = *data.get(*pos).ok_or(Error::Truncated {
            offset: base + *pos,
        })?;
        *pos += 1;
        if shift == 63 && byte > 1 {
            return Err(Error::VarintOverflow {
                offset: base + start,
            });
        }
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    Err(Error::VarintOverflow {
        offset: base + start,
    })
}

pub(crate) fn parse(data: &[u8], limits: Limits) -> Result<Message> {
    parse_as(data, limits, "BinaryPreset")
}

/// Parses `data` as the schema message `root` (for example `"Model"`).
pub(crate) fn parse_as(data: &[u8], limits: Limits, root: &str) -> Result<Message> {
    if data.len() > limits.max_bytes {
        return Err(Error::Limit {
            kind: "bytes",
            limit: limits.max_bytes,
        });
    }
    let mut budget = Budget { limits, fields: 0 };
    let mut pos = 0;
    let (message, _) = parse_message(
        data,
        &mut pos,
        0,
        0,
        Some(schema::message(root)),
        None,
        &mut budget,
    )?;
    Ok(message)
}

struct Budget {
    limits: Limits,
    fields: usize,
}

fn take<'a>(data: &'a [u8], pos: &mut usize, length: usize, base: usize) -> Result<&'a [u8]> {
    let end = pos.checked_add(length).ok_or(Error::LengthOverflow {
        offset: base + *pos,
    })?;
    let bytes = data.get(*pos..end).ok_or(Error::Truncated {
        offset: base + *pos,
    })?;
    *pos = end;
    Ok(bytes)
}

fn parse_message(
    data: &[u8],
    pos: &mut usize,
    base: usize,
    depth: usize,
    descriptor: Option<&'static schema::MessageSchema>,
    end_group: Option<u32>,
    budget: &mut Budget,
) -> Result<(Message, Raw)> {
    if depth > budget.limits.max_depth {
        return Err(Error::Limit {
            kind: "depth",
            limit: budget.limits.max_depth,
        });
    }
    let mut message = Message {
        fields: Vec::with_capacity(match end_group {
            // At most the fields the budget still allows: more fail the parse.
            None => count_fields(
                &data[*pos..],
                budget.limits.max_fields.saturating_sub(budget.fields),
            ),
            Some(_) => 0,
        }),
        schema: descriptor,
    };
    while *pos < data.len() {
        let start = *pos;
        let tag = read_varint(data, pos, base)?;
        let number = tag >> 3;
        let wire = (tag & 7) as u8;
        if number == 0 || number > 0x1fff_ffff {
            return Err(Error::InvalidFieldNumber {
                offset: base + start,
            });
        }
        let number = number as u32;
        if wire == 4 {
            if end_group == Some(number) {
                return Ok((message, Raw::new(&data[start..*pos])));
            }
            return Err(Error::InvalidGroup {
                offset: base + start,
            });
        }
        budget.fields += 1;
        if budget.fields > budget.limits.max_fields {
            return Err(Error::Limit {
                kind: "fields",
                limit: budget.limits.max_fields,
            });
        }
        let tag_bytes = Raw::new(&data[start..*pos]);
        let value = match wire {
            0 => {
                let start = *pos;
                let value = read_varint(data, pos, base)?;
                Value::Varint {
                    raw: Raw::new(&data[start..*pos]),
                    value,
                }
            }
            1 => Value::Fixed64(take(data, pos, 8, base)?.try_into().expect("eight bytes")),
            2 => {
                let start = *pos;
                let size = read_varint(data, pos, base)?;
                let size = usize::try_from(size).map_err(|_| Error::LengthOverflow {
                    offset: base + start,
                })?;
                let length = Raw::new(&data[start..*pos]);
                let payload_base = base + *pos;
                let payload = take(data, pos, size, base)?;
                if let Some(schema::FieldSchema {
                    kind: schema::Kind::Message(name),
                    ..
                }) = descriptor.and_then(|s| s.field(number))
                {
                    let mut child_pos = 0;
                    let (body, _) = parse_message(
                        payload,
                        &mut child_pos,
                        payload_base,
                        depth + 1,
                        Some(schema::message(name)),
                        None,
                        budget,
                    )?;
                    Value::Message { length, body }
                } else {
                    if let Some(spec) = descriptor.and_then(|s| s.field(number))
                        && spec.repeated
                        && spec.kind.packable()
                    {
                        count_packed(payload, spec.kind, budget)?;
                    }
                    Value::Bytes {
                        length,
                        data: payload.to_vec(),
                    }
                }
            }
            3 => {
                let (body, end) =
                    parse_message(data, pos, base, depth + 1, None, Some(number), budget)?;
                Value::Group { body, end }
            }
            5 => Value::Fixed32(take(data, pos, 4, base)?.try_into().expect("four bytes")),
            _ => {
                return Err(Error::InvalidWire {
                    offset: base + start,
                    wire,
                });
            }
        };
        message.fields.push(Field {
            number,
            tag: tag_bytes,
            value,
        });
    }
    if end_group.is_some() {
        return Err(Error::InvalidGroup {
            offset: base + *pos,
        });
    }
    Ok((message, Raw::default()))
}

/// The fields that `data` starts with, up to the first group or malformed field and at most
/// `limit`: the capacity of a message's field list, so the list does not grow field by field.
fn count_fields(data: &[u8], limit: usize) -> usize {
    let mut pos = 0;
    let mut count = 0;
    while pos < data.len() && count < limit {
        let Ok(tag) = read_varint(data, &mut pos, 0) else {
            break;
        };
        let skipped = match tag & 7 {
            0 => read_varint(data, &mut pos, 0).is_ok(),
            1 => take(data, &mut pos, 8, 0).is_ok(),
            2 => read_varint(data, &mut pos, 0)
                .ok()
                .and_then(|size| usize::try_from(size).ok())
                .is_some_and(|size| take(data, &mut pos, size, 0).is_ok()),
            5 => take(data, &mut pos, 4, 0).is_ok(),
            _ => false,
        };
        if !skipped {
            break;
        }
        count += 1;
    }
    count
}

// Account for packed elements before typed summaries allocate scalar lists.
// Malformed packed contents remain schema diagnostics, rather than wire errors.
fn count_packed(data: &[u8], kind: schema::Kind, budget: &mut Budget) -> Result<()> {
    let mut pos = 0;
    while pos < data.len() {
        if kind == schema::Kind::Float {
            if data.len() - pos < 4 {
                break;
            }
            pos += 4;
        } else if read_varint(data, &mut pos, 0).is_err() {
            break;
        }
        budget.fields += 1;
        if budget.fields > budget.limits.max_fields {
            return Err(Error::Limit {
                kind: "fields",
                limit: budget.limits.max_fields,
            });
        }
    }
    Ok(())
}

pub(crate) fn bytes_field(number: u32, bytes: Vec<u8>) -> Field {
    Field {
        number,
        tag: encode_varint(u64::from(number) << 3 | 2),
        value: Value::Bytes {
            length: encode_varint(bytes.len() as u64),
            data: bytes,
        },
    }
}
