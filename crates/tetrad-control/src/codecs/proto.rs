use super::types::{EnumValue, ProtoEnum};
use crate::Error;
use crate::scene::{MAX_SCENE_GROUP_DEPTH, SceneLimits, SceneWireError};
use crate::wire::Cursor;

use std::fmt;

pub type CodecResult<T> = Result<T, CodecError>;

#[derive(Debug, Clone, Copy)]
pub struct CodecLimits {
    pub max_payload_bytes: usize,
    /// Includes fields inside known nested messages and unknown groups.
    pub max_fields: usize,
    /// Combined known-message and unknown-group nesting, at most 64.
    pub max_depth: usize,
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_payload_bytes: 16 * 1024,
            max_fields: 1024,
            max_depth: 16,
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("payload exceeds {0} bytes")]
    PayloadLimit(usize),
    #[error("payload exceeds {0} fields")]
    FieldLimit(usize),
    #[error("payload exceeds nesting depth {0}")]
    DepthLimit(usize),
    #[error("configured nesting depth {0} exceeds 64")]
    DepthConfiguration(usize),
    #[error("invalid protobuf at local message byte {offset}: {problem}")]
    Wire {
        offset: usize,
        problem: SceneWireError,
    },
    #[error("field {field} value {value} exceeds uint32")]
    Integer { field: u32, value: u64 },
    /// proto3 string fields must be UTF-8; the stock parser also fails them.
    #[error("field {0} string is not valid UTF-8")]
    Utf8(u32),
    #[error("message type {0} has no codec in this slice")]
    UnsupportedMessageType(u32),
    #[error(transparent)]
    Envelope(Error),
}

impl From<Error> for CodecError {
    fn from(error: Error) -> Self {
        match error {
            Error::SceneFieldLimit(limit) => Self::FieldLimit(limit),
            Error::SceneDepthLimit(limit) => Self::DepthLimit(limit),
            Error::SceneWire { offset, problem } => Self::Wire { offset, problem },
            other => Self::Envelope(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireValue<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    /// The complete group (including both keys) is retained in WireField::raw.
    Group,
    Fixed32(u32),
}

/// One top-level field occurrence. Known fields with a wrong wire type are
/// retained here but do not supply typed values. Nested raw payloads are in Bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireField<'a> {
    pub number: u32,
    pub value: WireValue<'a>,
    pub raw: &'a [u8],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decoded<'a, T> {
    pub message: T,
    fields: Vec<WireField<'a>>,
    original: &'a [u8],
}

impl<'a, T> Decoded<'a, T> {
    /// Unchanged source bytes, independent of any edits to `message`.
    pub fn original_payload(&self) -> &'a [u8] {
        self.original
    }

    pub fn fields(&self) -> &[WireField<'a>] {
        &self.fields
    }

    pub(crate) fn map<U>(self, map: impl FnOnce(T) -> U) -> Decoded<'a, U> {
        Decoded {
            message: map(self.message),
            fields: self.fields,
            original: self.original,
        }
    }
}

fn validate_limits(limits: CodecLimits) -> CodecResult<()> {
    if limits.max_depth > MAX_SCENE_GROUP_DEPTH {
        return Err(CodecError::DepthConfiguration(limits.max_depth));
    }
    Ok(())
}

pub(crate) trait Message: Default {
    fn merge(
        &mut self,
        fields: &[WireField<'_>],
        decoder: &mut Decoder,
        depth: usize,
    ) -> CodecResult<()>;
    fn write(&self, encoder: &mut Encoder) -> CodecResult<()>;
}

pub(crate) fn decode<T: Message>(
    payload: &[u8],
    limits: CodecLimits,
) -> CodecResult<Decoded<'_, T>> {
    validate_limits(limits)?;
    if payload.len() > limits.max_payload_bytes {
        return Err(CodecError::PayloadLimit(limits.max_payload_bytes));
    }
    let mut decoder = Decoder { limits, fields: 0 };
    let fields = decoder.scan(payload, 0)?;
    let mut message = T::default();
    message.merge(&fields, &mut decoder, 0)?;
    Ok(Decoded {
        message,
        fields,
        original: payload,
    })
}

pub(crate) struct Decoder {
    limits: CodecLimits,
    fields: usize,
}

impl Decoder {
    pub(crate) fn scan<'a>(
        &mut self,
        payload: &'a [u8],
        depth: usize,
    ) -> CodecResult<Vec<WireField<'a>>> {
        if depth > self.limits.max_depth {
            return Err(CodecError::DepthLimit(self.limits.max_depth));
        }
        let mut cursor = Cursor {
            payload,
            offset: 0,
            fields: self.fields,
            limits: SceneLimits {
                max_payload_bytes: self.limits.max_payload_bytes,
                max_fields: self.limits.max_fields,
                max_group_depth: self.limits.max_depth - depth,
            },
        };
        let mut fields = Vec::new();
        while cursor.offset < payload.len() {
            let start = cursor.offset;
            let (number, wire) = cursor.key()?;
            let value = match wire {
                0 => WireValue::Varint(cursor.varint()?),
                1 => WireValue::Fixed64(u64::from_le_bytes(
                    cursor.take(8)?.try_into().expect("eight bytes"),
                )),
                2 => WireValue::Bytes(cursor.bytes()?),
                5 => WireValue::Fixed32(u32::from_le_bytes(
                    cursor.take(4)?.try_into().expect("four bytes"),
                )),
                _ => {
                    cursor.skip(number, wire, 0).map_err(|error| match error {
                        Error::SceneDepthLimit(_) => CodecError::DepthLimit(self.limits.max_depth),
                        other => other.into(),
                    })?;
                    WireValue::Group
                }
            };
            fields.push(WireField {
                number,
                value,
                raw: &payload[start..cursor.offset],
            });
        }
        self.fields = cursor.fields;
        Ok(fields)
    }

    /// Packed repeated numerics (proto3 default): a length-delimited occurrence
    /// holds zero or more elements, any other correct wire type holds one.
    /// Every element, packed or unpacked, costs one unit of `max_fields` in
    /// addition to its occurrence key; a wrong-wire occurrence costs only the
    /// key. Wire errors report the offset inside the packed body.
    pub(crate) fn packed<T: Packed>(
        &mut self,
        field: &WireField<'_>,
        slot: &mut Vec<T>,
    ) -> CodecResult<()> {
        let WireValue::Bytes(body) = field.value else {
            if let Some(value) = T::read(field)? {
                self.element()?;
                slot.push(value);
            }
            return Ok(());
        };
        let mut cursor = Cursor {
            payload: body,
            offset: 0,
            fields: 0,
            limits: SceneLimits {
                max_payload_bytes: self.limits.max_payload_bytes,
                max_fields: self.limits.max_fields,
                max_group_depth: 0,
            },
        };
        while cursor.offset < body.len() {
            self.element()?;
            slot.push(T::read_packed(&mut cursor, field.number)?);
        }
        Ok(())
    }

    fn element(&mut self) -> CodecResult<()> {
        if self.fields >= self.limits.max_fields {
            return Err(CodecError::FieldLimit(self.limits.max_fields));
        }
        self.fields += 1;
        Ok(())
    }
}

pub(crate) fn encode<T: Message>(message: &T, limits: CodecLimits) -> CodecResult<Vec<u8>> {
    validate_limits(limits)?;
    let mut encoder = Encoder {
        bytes: Vec::new(),
        limits,
        room: limits.max_payload_bytes,
        fields: 0,
        depth: 0,
    };
    message.write(&mut encoder)?;
    Ok(encoder.bytes)
}

pub(crate) struct Encoder {
    bytes: Vec<u8>,
    limits: CodecLimits,
    /// Most bytes this buffer may hold. For a nested buffer this is the room
    /// left in its parent after the key and a one-byte length prefix, so a
    /// nested write fails as soon as the whole payload can no longer fit.
    /// Invariant: `bytes.len() <= room`.
    room: usize,
    fields: usize,
    depth: usize,
}

impl Encoder {
    fn payload_limit(&self) -> CodecError {
        CodecError::PayloadLimit(self.limits.max_payload_bytes)
    }

    fn remaining(&self) -> usize {
        self.room - self.bytes.len()
    }

    fn append(&mut self, bytes: &[u8]) -> CodecResult<()> {
        if bytes.len() > self.remaining() {
            return Err(self.payload_limit());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn varint(&mut self, value: u64) -> CodecResult<()> {
        let (bytes, length) = varint_bytes(value);
        self.append(&bytes[..length])
    }

    fn field(&mut self, number: u32, wire: u8) -> CodecResult<()> {
        if self.fields >= self.limits.max_fields {
            return Err(CodecError::FieldLimit(self.limits.max_fields));
        }
        self.fields += 1;
        self.varint((u64::from(number) << 3) | u64::from(wire))
    }

    pub(crate) fn bytes(&mut self, number: u32, bytes: &[u8]) -> CodecResult<()> {
        self.field(number, 2)?;
        self.varint(bytes.len() as u64)?;
        self.append(bytes)
    }

    /// Writes a non-empty list in packed form. Each element costs one unit of
    /// `max_fields`, matching `Decoder::packed`; an empty list writes nothing.
    /// No scratch body is built. Every element needs at least one byte and
    /// the length prefix at least one, so a list longer than the room fails
    /// before any element is read. Otherwise sizing stops once the body cannot
    /// fit, and the exact key + prefix + body total is checked before the
    /// output grows by exactly that amount.
    pub(crate) fn packed<T: Packed>(&mut self, number: u32, values: &[T]) -> CodecResult<()> {
        if values.is_empty() {
            return Ok(());
        }
        self.field(number, 2)?;
        self.fields = self
            .fields
            .checked_add(values.len())
            .filter(|fields| *fields <= self.limits.max_fields)
            .ok_or(CodecError::FieldLimit(self.limits.max_fields))?;
        let remaining = self.remaining();
        if values.len() >= remaining {
            return Err(self.payload_limit());
        }
        // Cannot overflow: `body < remaining` before each add of at most 10.
        let mut body = 0;
        for value in values {
            body += value.packed_bytes().1;
            if body >= remaining {
                return Err(self.payload_limit());
            }
        }
        let (prefix, prefix_length) = varint_bytes(body as u64);
        let total = prefix_length + body;
        if total > remaining {
            return Err(self.payload_limit());
        }
        self.bytes.reserve_exact(total);
        self.bytes.extend_from_slice(&prefix[..prefix_length]);
        for value in values {
            let (bytes, length) = value.packed_bytes();
            self.bytes.extend_from_slice(&bytes[..length]);
        }
        Ok(())
    }

    /// Writes exact raw message bytes after the same shallow scan used on
    /// decode: top-level wire validity at the nested depth, with every
    /// top-level key and group key charged to `max_fields`. Deeper
    /// length-delimited contents stay opaque, as on decode. The byte cap is
    /// checked before the scan, as decode checks payload length first.
    pub(crate) fn raw_message(&mut self, number: u32, raw: &RawMessageBytes) -> CodecResult<()> {
        self.field(number, 2)?;
        self.varint(raw.bytes.len() as u64)?;
        if raw.bytes.len() > self.remaining() {
            return Err(self.payload_limit());
        }
        let mut decoder = Decoder {
            limits: self.limits,
            fields: self.fields,
        };
        decoder.scan(&raw.bytes, self.depth + 1)?;
        self.fields = decoder.fields;
        self.append(&raw.bytes)
    }

    /// Writes the body in place after a one-byte length placeholder; a body of 128 bytes or
    /// more moves up to make room for its longer prefix. The nested writer shares this
    /// buffer and its room, so the body may take every byte but the placeholder. (A scratch
    /// buffer per nested message cost an allocation and a copy per level: 120 µs for the
    /// 37 KB ModelPreset READ list on the host, A3.)
    pub(crate) fn message<T: Message>(&mut self, number: u32, message: &T) -> CodecResult<()> {
        if self.depth >= self.limits.max_depth {
            return Err(CodecError::DepthLimit(self.limits.max_depth));
        }
        self.field(number, 2)?;
        if self.remaining() == 0 {
            return Err(self.payload_limit());
        }
        self.bytes.push(0);
        let start = self.bytes.len();
        let mut nested = Self {
            bytes: std::mem::take(&mut self.bytes),
            limits: self.limits,
            room: self.room,
            fields: self.fields,
            depth: self.depth + 1,
        };
        let written = message.write(&mut nested);
        self.bytes = nested.bytes;
        self.fields = nested.fields;
        written?;
        let (prefix, length) = varint_bytes((self.bytes.len() - start) as u64);
        if length - 1 > self.remaining() {
            return Err(self.payload_limit());
        }
        self.bytes
            .splice(start - 1..start, prefix[..length].iter().copied());
        Ok(())
    }
}

fn varint_bytes(mut value: u64) -> ([u8; 10], usize) {
    let mut bytes = [0; 10];
    let mut length = 0;
    loop {
        bytes[length] = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            bytes[length] |= 0x80;
        }
        length += 1;
        if value == 0 {
            break;
        }
    }
    (bytes, length)
}

/// A nested message kept as exact wire bytes because its schema is owned by
/// another crate. Duplicate occurrences concatenate, which is protobuf merge
/// semantics for a singular message field. Decode and encode both check only
/// top-level wire validity, nesting depth and top-level field/group budgets;
/// deeper validation belongs to the owner.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RawMessageBytes {
    pub bytes: Vec<u8>,
}

impl fmt::Debug for RawMessageBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RawMessageBytes({} bytes)", self.bytes.len())
    }
}

/// Scalars that may appear in packed repeated fields.
pub(crate) trait Packed: Scalar {
    fn read_packed(cursor: &mut Cursor<'_>, number: u32) -> CodecResult<Self>;
    /// One packed element as `(buffer, length)`; only `buffer[..length]` is written.
    fn packed_bytes(&self) -> ([u8; 10], usize);
}

pub(crate) trait Scalar: Sized {
    fn read(field: &WireField<'_>) -> CodecResult<Option<Self>>;
    fn write(&self, number: u32, encoder: &mut Encoder) -> CodecResult<()>;
}

impl Scalar for u64 {
    fn read(field: &WireField<'_>) -> CodecResult<Option<Self>> {
        Ok(match field.value {
            WireValue::Varint(value) => Some(value),
            _ => None,
        })
    }

    fn write(&self, number: u32, encoder: &mut Encoder) -> CodecResult<()> {
        encoder.field(number, 0)?;
        encoder.varint(*self)
    }
}

impl Scalar for u32 {
    fn read(field: &WireField<'_>) -> CodecResult<Option<Self>> {
        match field.value {
            WireValue::Varint(value) => Ok(Some(u32::try_from(value).map_err(|_| {
                CodecError::Integer {
                    field: field.number,
                    value,
                }
            })?)),
            _ => Ok(None),
        }
    }

    fn write(&self, number: u32, encoder: &mut Encoder) -> CodecResult<()> {
        u64::from(*self).write(number, encoder)
    }
}

impl Scalar for i32 {
    fn read(field: &WireField<'_>) -> CodecResult<Option<Self>> {
        // Protobuf int32/enum readers use the low 32 bits; negatives can occupy
        // ten bytes. The original varint remains in WireField for exact replay.
        Ok(match field.value {
            WireValue::Varint(value) => Some(value as i32),
            _ => None,
        })
    }

    fn write(&self, number: u32, encoder: &mut Encoder) -> CodecResult<()> {
        (i64::from(*self) as u64).write(number, encoder)
    }
}

impl Scalar for bool {
    fn read(field: &WireField<'_>) -> CodecResult<Option<Self>> {
        Ok(match field.value {
            WireValue::Varint(value) => Some(value != 0),
            _ => None,
        })
    }

    fn write(&self, number: u32, encoder: &mut Encoder) -> CodecResult<()> {
        u64::from(*self).write(number, encoder)
    }
}

impl Scalar for f32 {
    fn read(field: &WireField<'_>) -> CodecResult<Option<Self>> {
        Ok(match field.value {
            WireValue::Fixed32(bits) => Some(Self::from_bits(bits)),
            _ => None,
        })
    }

    fn write(&self, number: u32, encoder: &mut Encoder) -> CodecResult<()> {
        encoder.field(number, 5)?;
        encoder.append(&self.to_bits().to_le_bytes())
    }
}

/// proto3 `bytes`: any length-delimited content, including invalid UTF-8.
impl Scalar for Vec<u8> {
    fn read(field: &WireField<'_>) -> CodecResult<Option<Self>> {
        Ok(match field.value {
            WireValue::Bytes(bytes) => Some(bytes.to_vec()),
            _ => None,
        })
    }

    fn write(&self, number: u32, encoder: &mut Encoder) -> CodecResult<()> {
        encoder.bytes(number, self)
    }
}

/// proto3 `string`: invalid UTF-8 is a decode error, not a lossy conversion.
impl Scalar for String {
    fn read(field: &WireField<'_>) -> CodecResult<Option<Self>> {
        match field.value {
            WireValue::Bytes(bytes) => std::str::from_utf8(bytes)
                .map(|text| Some(text.to_owned()))
                .map_err(|_| CodecError::Utf8(field.number)),
            _ => Ok(None),
        }
    }

    fn write(&self, number: u32, encoder: &mut Encoder) -> CodecResult<()> {
        encoder.bytes(number, self.as_bytes())
    }
}

impl<E: ProtoEnum> Scalar for EnumValue<E> {
    fn read(field: &WireField<'_>) -> CodecResult<Option<Self>> {
        Ok(i32::read(field)?.map(E::from_number))
    }

    fn write(&self, number: u32, encoder: &mut Encoder) -> CodecResult<()> {
        self.number().write(number, encoder)
    }
}

impl Scalar for i64 {
    fn read(field: &WireField<'_>) -> CodecResult<Option<Self>> {
        Ok(match field.value {
            WireValue::Varint(value) => Some(value as i64),
            _ => None,
        })
    }

    fn write(&self, number: u32, encoder: &mut Encoder) -> CodecResult<()> {
        (*self as u64).write(number, encoder)
    }
}

impl Packed for u64 {
    fn read_packed(cursor: &mut Cursor<'_>, _number: u32) -> CodecResult<Self> {
        Ok(cursor.varint()?)
    }

    fn packed_bytes(&self) -> ([u8; 10], usize) {
        varint_bytes(*self)
    }
}

impl Packed for u32 {
    fn read_packed(cursor: &mut Cursor<'_>, number: u32) -> CodecResult<Self> {
        let value = cursor.varint()?;
        u32::try_from(value).map_err(|_| CodecError::Integer {
            field: number,
            value,
        })
    }

    fn packed_bytes(&self) -> ([u8; 10], usize) {
        varint_bytes(u64::from(*self))
    }
}

impl Packed for i32 {
    fn read_packed(cursor: &mut Cursor<'_>, _number: u32) -> CodecResult<Self> {
        Ok(cursor.varint()? as i32)
    }

    fn packed_bytes(&self) -> ([u8; 10], usize) {
        varint_bytes(i64::from(*self) as u64)
    }
}

impl Packed for i64 {
    fn read_packed(cursor: &mut Cursor<'_>, _number: u32) -> CodecResult<Self> {
        Ok(cursor.varint()? as i64)
    }

    fn packed_bytes(&self) -> ([u8; 10], usize) {
        varint_bytes(*self as u64)
    }
}

impl Packed for bool {
    fn read_packed(cursor: &mut Cursor<'_>, _number: u32) -> CodecResult<Self> {
        Ok(cursor.varint()? != 0)
    }

    fn packed_bytes(&self) -> ([u8; 10], usize) {
        varint_bytes(u64::from(*self))
    }
}

impl Packed for f32 {
    fn read_packed(cursor: &mut Cursor<'_>, _number: u32) -> CodecResult<Self> {
        let bits = cursor.take(4)?;
        Ok(Self::from_bits(u32::from_le_bytes(
            bits.try_into().expect("four bytes"),
        )))
    }

    fn packed_bytes(&self) -> ([u8; 10], usize) {
        let mut bytes = [0; 10];
        bytes[..4].copy_from_slice(&self.to_bits().to_le_bytes());
        (bytes, 4)
    }
}

impl<E: ProtoEnum> Packed for EnumValue<E> {
    fn read_packed(cursor: &mut Cursor<'_>, number: u32) -> CodecResult<Self> {
        Ok(E::from_number(i32::read_packed(cursor, number)?))
    }

    fn packed_bytes(&self) -> ([u8; 10], usize) {
        self.number().packed_bytes()
    }
}
