use crate::{Error, Limits, Result, schema};

#[derive(Debug, Clone)]
pub(crate) struct Message {
    pub fields: Vec<Field>,
    pub schema: Option<&'static schema::MessageSchema>,
}

#[derive(Debug, Clone)]
pub(crate) struct Field {
    pub number: u32,
    pub tag: Vec<u8>,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub(crate) enum Value {
    Varint { raw: Vec<u8>, value: u64 },
    Fixed64([u8; 8]),
    Bytes { length: Vec<u8>, data: Vec<u8> },
    Message { length: Vec<u8>, body: Message },
    Group { body: Message, end: Vec<u8> },
    Fixed32([u8; 4]),
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

pub(crate) fn encode_varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(10);
    while value > 127 {
        bytes.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    bytes.push(value as u8);
    bytes
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
        Some(schema::message("BinaryPreset")),
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
) -> Result<(Message, Vec<u8>)> {
    if depth > budget.limits.max_depth {
        return Err(Error::Limit {
            kind: "depth",
            limit: budget.limits.max_depth,
        });
    }
    let mut message = Message {
        fields: Vec::new(),
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
                return Ok((message, data[start..*pos].to_vec()));
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
        let tag_bytes = data[start..*pos].to_vec();
        let value = match wire {
            0 => {
                let start = *pos;
                let value = read_varint(data, pos, base)?;
                Value::Varint {
                    raw: data[start..*pos].to_vec(),
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
                let length = data[start..*pos].to_vec();
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
    Ok((message, Vec::new()))
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
