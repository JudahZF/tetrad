//! Bounded protobuf wire reader and writer for the small media messages.

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProtoError {
    #[error("protobuf input truncated at byte {0}")]
    Truncated(usize),
    #[error("protobuf varint at byte {0} is longer than 10 bytes")]
    Varint(usize),
    #[error("unsupported protobuf wire type {wire_type} at byte {offset}")]
    WireType { wire_type: u8, offset: usize },
    #[error("protobuf field {0} is not valid UTF-8")]
    Utf8(u32),
    #[error("protobuf field {field} has wire type {actual}, expected {expected}")]
    FieldType {
        field: u32,
        actual: u8,
        expected: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
    Fixed64(u64),
}

impl<'a> Value<'a> {
    fn wire_type(self) -> u8 {
        match self {
            Self::Varint(_) => 0,
            Self::Fixed64(_) => 1,
            Self::Bytes(_) => 2,
            Self::Fixed32(_) => 5,
        }
    }

    pub(crate) fn bytes(self, field: u32) -> Result<&'a [u8], ProtoError> {
        match self {
            Self::Bytes(bytes) => Ok(bytes),
            other => Err(ProtoError::FieldType {
                field,
                actual: other.wire_type(),
                expected: 2,
            }),
        }
    }

    pub(crate) fn string(self, field: u32) -> Result<&'a str, ProtoError> {
        std::str::from_utf8(self.bytes(field)?).map_err(|_| ProtoError::Utf8(field))
    }
}

/// Iterates `(field, value)` pairs. Groups (wire types 3/4) are refused.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn varint(&mut self) -> Result<u64, ProtoError> {
        let start = self.offset;
        let mut value = 0_u64;
        for shift in (0..70).step_by(7) {
            let byte = *self
                .bytes
                .get(self.offset)
                .ok_or(ProtoError::Truncated(self.offset))?;
            self.offset += 1;
            value |= u64::from(byte & 0x7f) << shift.min(63);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(ProtoError::Varint(start))
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ProtoError> {
        let end = self
            .offset
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(ProtoError::Truncated(self.offset))?;
        let bytes = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(bytes)
    }

    fn field(&mut self) -> Result<(u32, Value<'a>), ProtoError> {
        let offset = self.offset;
        let key = self.varint()?;
        let field = u32::try_from(key >> 3).map_err(|_| ProtoError::Varint(offset))?;
        let value = match key & 7 {
            0 => Value::Varint(self.varint()?),
            1 => Value::Fixed64(u64::from_le_bytes(
                self.take(8)?.try_into().expect("8 bytes"),
            )),
            2 => {
                let len = usize::try_from(self.varint()?)
                    .map_err(|_| ProtoError::Truncated(self.offset))?;
                Value::Bytes(self.take(len)?)
            }
            5 => Value::Fixed32(u32::from_le_bytes(
                self.take(4)?.try_into().expect("4 bytes"),
            )),
            wire_type => {
                return Err(ProtoError::WireType {
                    wire_type: wire_type as u8,
                    offset,
                });
            }
        };
        Ok((field, value))
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Result<(u32, Value<'a>), ProtoError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.bytes.len() {
            return None;
        }
        let item = self.field();
        if item.is_err() {
            self.offset = self.bytes.len();
        }
        Some(item)
    }
}

pub(crate) fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

pub(crate) fn put_bytes(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    put_varint(out, u64::from(field) << 3 | 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}
