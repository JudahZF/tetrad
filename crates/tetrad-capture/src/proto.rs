//! Minimal protobuf wire reader over one message. Stock parses `captureProto.Capture` (V1,
//! VA 0x6ec290) and `snng_obfuscate.ObfuscatedGraph` (V2, VA 0x6f2b60) with libprotobuf.

/// The bytes are not a valid protobuf message, or a known field has the wrong wire type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("malformed protobuf message")]
pub struct Malformed;

pub(crate) enum Value<'a> {
    Varint(u64),
    Fixed64,
    Bytes(&'a [u8]),
    Fixed32(u32),
}

/// `(field number, value)` in wire order. The first error ends the iteration.
pub(crate) struct Fields<'a>(pub(crate) &'a [u8]);

impl<'a> Fields<'a> {
    fn varint(&mut self) -> Result<u64, Malformed> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self.0.split_first().ok_or(Malformed)?;
            self.0 = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(Malformed)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Malformed> {
        if n > self.0.len() {
            return Err(Malformed);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn field(&mut self) -> Result<(u64, Value<'a>), Malformed> {
        let key = self.varint()?;
        let value = match key & 7 {
            0 => Value::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Value::Fixed64
            }
            2 => {
                let n = usize::try_from(self.varint()?).map_err(|_| Malformed)?;
                Value::Bytes(self.take(n)?)
            }
            5 => Value::Fixed32(u32::from_le_bytes(
                self.take(4)?.try_into().expect("four bytes"),
            )),
            _ => return Err(Malformed),
        };
        Ok((key >> 3, value))
    }

    /// Every element of a packed repeated varint field.
    pub(crate) fn packed_varints(bytes: &'a [u8]) -> impl Iterator<Item = Result<u64, Malformed>> {
        let mut reader = Fields(bytes);
        std::iter::from_fn(move || (!reader.0.is_empty()).then(|| reader.varint()))
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Value<'a>), Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }
        let field = self.field();
        if field.is_err() {
            self.0 = &[];
        }
        Some(field)
    }
}
