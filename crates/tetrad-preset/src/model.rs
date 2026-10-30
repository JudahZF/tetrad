//! Typed, editable mirror of the recovered schema with stock protobuf semantics.
//!
//! ZenUI links `libprotobuf.so.29` (protobuf 3.18). Decoding follows its generated
//! parser: the last scalar occurrence wins, packed and unpacked repeated scalars
//! are both accepted, a known number with a foreign wire type is kept as an
//! unknown field, integers truncate to 32 bits and strings must be UTF-8.
//! Encoding follows its serializer: ascending field numbers, packed repeated
//! scalars, proto3 `optional` fields by presence, other scalars only when nonzero
//! (floats by raw bits), and unknown fields last in their original order.
//! All 262 factory presets and 94 user presets saved by CorOS 1.0-4.1 re-encode
//! byte for byte (`examples/corpus_audit.rs`).
//! Stock map order follows its hash table and is not reproducible, so maps keep
//! their decoded entry order and append new keys. Entries carry key and value.

use crate::{
    Error, Limits, ParamValue, PresetDocument, Result,
    wire::{self, Field, Message, Value},
};

/// Unknown fields of one message, as raw tag/value bytes in received order.
/// Stock re-serializes them after the known fields; so does [`Preset::to_bytes`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unknown(Vec<u8>);

impl Unknown {
    pub const EMPTY: Self = Self(Vec::new());

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn push(&mut self, field: &Field) {
        self.0.extend_from_slice(&field.tag);
        field.value.write(&mut self.0);
    }

    /// The payload of the last length-delimited field `number`, as a protobuf reader keeps the
    /// last occurrence.
    pub fn bytes_field(&self, number: u32) -> Option<Vec<u8>> {
        let message = wire::parse(&self.0, Limits::default()).ok()?;
        let field = message.fields.iter().rev().find(|f| f.number == number)?;
        match &field.value {
            Value::Bytes { data, .. } => Some(data.clone()),
            Value::Message { body, .. } => {
                let mut out = Vec::new();
                body.write(&mut out);
                Some(out)
            }
            _ => None,
        }
    }

    /// Replaces every field `number` with one bytes field at the end, or removes it. The other
    /// unknown fields keep their bytes and order.
    pub fn set_bytes_field(&mut self, number: u32, bytes: Option<Vec<u8>>) -> Result<()> {
        let mut message = wire::parse(&self.0, Limits::default())?;
        message.fields.retain(|f| f.number != number);
        message
            .fields
            .extend(bytes.map(|bytes| wire::bytes_field(number, bytes)));
        self.0.clear();
        message.write(&mut self.0);
        Ok(())
    }
}

/// Owned (Tetrad): the preset field that holds the per-preset mappings (UTF-8 JSON). Stock
/// does not know it and keeps it as an unknown field when it saves.
pub const TETRAD_MAPPINGS_FIELD: u32 = 7001;

/// Owned (Tetrad): the preset field that holds the community block records (UTF-8 JSON, see
/// [`crate::community`]). Stock does not know it and keeps it as an unknown field when it saves.
pub const TETRAD_BLOCKS_FIELD: u32 = 7002;

/// Owned (Tetrad): the preset field that holds the preset's own Custom footswitch mode layout
/// (UTF-8 JSON, the layout file's format). Stock does not know it and keeps it as an unknown
/// field when it saves.
pub const TETRAD_FOOTSWITCH_FIELD: u32 = 7003;

/// A protobuf map that keeps stored entry order. A repeated key keeps its
/// first position and its last value, as the stock map parser keeps one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapField<V>(Vec<(u32, V)>);

impl<V> Default for MapField<V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<V> MapField<V> {
    pub fn get(&self, key: u32) -> Option<&V> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// Replaces a value in place or appends a new entry.
    pub fn insert(&mut self, key: u32, value: V) {
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key, value)),
        }
    }

    pub fn remove(&mut self, key: u32) -> Option<V> {
        let position = self.0.iter().position(|(k, _)| *k == key)?;
        Some(self.0.remove(position).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, &V)> {
        self.0.iter().map(|(k, v)| (*k, v))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `BinaryPreset`. Proto3 `optional` scalars are `Option`; absence and an
/// explicit default are different states and both survive a save.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Preset {
    pub name: Option<String>,
    /// Stored identifier (see `tetrad_core::PresetId`), not a checksum.
    pub hash: Option<u32>,
    pub date: Option<String>,
    pub volume: Option<f32>,
    pub pan: Option<f32>,
    pub default_scene: Option<u32>,
    pub author_name: Option<String>,
    pub author_id: Option<String>,
    pub tempo: Option<u32>,
    pub chains: Vec<Chain>,
    pub tags: Vec<String>,
    pub legacy_stomp_mode_stomp_data: Vec<GridRef>,
    pub scene_tempo: Vec<u32>,
    pub scene_labels: Vec<String>,
    pub midi_messages: Vec<MidiMessage>,
    pub midi_messages_general: Vec<MidiMessage>,
    pub bypass: Vec<BypassRow>,
    pub tempo_program_data: Vec<Model>,
    pub layout_code_1: Vec<u32>,
    pub layout_code_2: Vec<u32>,
    pub created_version: Vec<String>,
    pub modified_version: Vec<String>,
    pub oldest_compatible_version: Vec<String>,
    pub cloud_id: Vec<String>,
    pub description: Vec<String>,
    pub stomp_mode_assignments: Vec<StompAssignment>,
    pub model_update_notifications: Vec<GridRef>,
    pub recompile_code_1: Vec<u32>,
    pub recompile_code_2: Vec<u32>,
    /// Raw stored colors; stock writes eight packed values.
    pub scene_colors: Vec<u32>,
    pub stomp_labels: MapField<String>,
    pub midi_messages_general_v2: Vec<MidiMessage>,
    pub single_stomp_labels: MapField<String>,
    pub stomp_is_momentary: MapField<bool>,
    pub side_chain_follow_exists: Option<bool>,
    pub zsk_7182_applied: Option<bool>,
    pub unknown: Unknown,
}

/// One grid row. Stock files hold four chains of eight `models` slots.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chain {
    pub in_portid: Option<u32>,
    pub out_portid: Option<u32>,
    pub models: Vec<Model>,
    pub split_control_points: Vec<SplitControlPoints>,
    pub splitter: Vec<Model>,
    pub mixer: Vec<Model>,
    pub output_control: Vec<Model>,
    pub split_bypass: Vec<SceneBypass>,
    pub mix_bypass: Vec<SceneBypass>,
    pub combined_splitter: Vec<Model>,
    pub input_control: Vec<Model>,
    pub row: Option<u32>,
    pub unknown: Unknown,
}

/// A model instance. `hash` is the catalog model ID; 0 marks an empty slot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Model {
    pub hash: Option<u32>,
    pub params: Vec<Param>,
    pub bypass_expression: Vec<Expression>,
    pub expression_bypass_info: Vec<ExpressionBypassInfo>,
    pub column: Option<u32>,
    pub notify_flag: Option<bool>,
    pub sidechain_sink_flag: Option<bool>,
    pub sidechain_source_flag: Option<bool>,
    pub unknown: Unknown,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Param {
    /// Opaque expression assignment code; stock unassigned value is 0.
    pub expression: Option<i32>,
    pub expression_min: Option<f32>,
    pub expression_max: Option<f32>,
    pub scene_mode: Option<bool>,
    /// One entry per scene; stock writes eight.
    pub values: Vec<SceneValue>,
    pub index: Option<u32>,
    pub is_led: Option<bool>,
    pub dynamic_steps: Vec<String>,
    pub dynamic_icons: Vec<String>,
    pub dynamic_metadata: Vec<String>,
    pub unknown: Unknown,
}

/// `ParamValue` message: `None` is an empty oneof.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneValue {
    pub value: Option<ParamValue>,
    pub unknown: Unknown,
}

/// Split/mix columns; stock writes -1 for "no junction".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SplitControlPoints {
    pub split: i32,
    pub mix: i32,
    pub unknown: Unknown,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BypassRow {
    pub col_bypass: Vec<ColBypass>,
    pub row: Option<u32>,
    pub unknown: Unknown,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColBypass {
    pub scene_bypass: Vec<SceneBypass>,
    pub column: Option<u32>,
    pub scene_mode: Option<bool>,
    pub unknown: Unknown,
}

/// Stored bool only. Stock copies it to the model's last runtime parameter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneBypass {
    pub bypass: bool,
    pub unknown: Unknown,
}

/// `LegacyStompModeStompData` and `SlotNotification` share this shape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GridRef {
    pub row: u32,
    pub column: u32,
    pub unknown: Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StompRole {
    Primary,
    Secondary,
    /// Proto3 enums are open; unknown values survive a save.
    Other(i32),
}

impl StompRole {
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            0 => Self::Primary,
            1 => Self::Secondary,
            other => Self::Other(other),
        }
    }

    pub fn raw(self) -> i32 {
        match self {
            Self::Primary => 0,
            Self::Secondary => 1,
            Self::Other(raw) => raw,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StompAssignment {
    pub row: u32,
    pub column: u32,
    pub stomp_index: u32,
    pub role: Option<StompRole>,
    pub unknown: Unknown,
}

/// `MidiMessageInfo`; stock files hold fixed lists of all-zero entries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MidiMessage {
    pub kind: u32,
    pub channel: u32,
    pub param1: u32,
    pub param2: u32,
    pub param3: u32,
    pub unknown: Unknown,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Expression {
    pub expression: i32,
    pub expression_min: f32,
    pub expression_max: f32,
    pub unknown: Unknown,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExpressionBypassInfo {
    pub kind: u32,
    pub invert: bool,
    pub delay_ms: u32,
    pub latch_emulation: bool,
    pub unknown: Unknown,
}

impl Preset {
    /// Bounded parse plus typed decode with stock parser semantics.
    pub fn decode(bytes: &[u8], limits: Limits) -> Result<Self> {
        Self::try_from(&PresetDocument::parse(bytes, limits)?)
    }

    /// Stock serializer layout. Byte-identical to stock for canonical input.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

impl Model {
    /// Bounded parse of one `Model` message with stock parser semantics.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::decode_message(&wire::parse_as(bytes, Limits::default(), "Model")?)
    }

    /// Stock serializer layout of one `Model` message. Unknown fields are kept.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

impl TryFrom<&PresetDocument> for Preset {
    type Error = Error;

    fn try_from(document: &PresetDocument) -> Result<Self> {
        Self::decode_message(document.root())
    }
}

// ---------------------------------------------------------------------------
// Decoding from the bounded wire tree.

trait Decode: Sized {
    fn decode_message(message: &Message) -> Result<Self>;
}

fn invalid(message: &'static str, field: u32, reason: &'static str) -> Error {
    Error::InvalidKnownField {
        message,
        field,
        reason,
    }
}

fn text(data: &[u8], message: &'static str, field: u32) -> Result<String> {
    String::from_utf8(data.to_vec()).map_err(|_| invalid(message, field, "invalid UTF-8"))
}

/// Unpacked varints truncate to u32 as stock does; packed payloads must parse.
fn uints(out: &mut Vec<u32>, value: &Value, message: &'static str, field: u32) -> Result<()> {
    match value {
        Value::Varint { value, .. } => out.push(*value as u32),
        Value::Bytes { data, .. } => {
            let mut pos = 0;
            while pos < data.len() {
                let value = wire::read_varint(data, &mut pos, 0)
                    .map_err(|_| invalid(message, field, "invalid packed varints"))?;
                out.push(value as u32);
            }
        }
        _ => unreachable!("caller matches varint or bytes"),
    }
    Ok(())
}

fn children<T: Decode>(out: &mut Vec<T>, body: &Message) -> Result<()> {
    out.push(T::decode_message(body)?);
    Ok(())
}

/// Map entries: missing key/value read as defaults; entry unknowns are dropped
/// and a repeated key keeps its last value, as the stock map parser does.
fn entry(body: &Message) -> (u32, Option<&Value>) {
    let mut key = 0;
    let mut value = None;
    for field in &body.fields {
        match (field.number, &field.value) {
            (1, Value::Varint { value, .. }) => key = *value as u32,
            (2, other) => value = Some(other),
            _ => {}
        }
    }
    (key, value)
}

fn string_entry(body: &Message, map: &'static str) -> Result<(u32, String)> {
    let (key, value) = entry(body);
    let value = match value {
        Some(Value::Bytes { data, .. }) => text(data, map, 2)?,
        _ => String::new(),
    };
    Ok((key, value))
}

impl Decode for Preset {
    fn decode_message(m: &Message) -> Result<Self> {
        const M: &str = "BinaryPreset";
        let mut p = Self::default();
        for f in &m.fields {
            let n = f.number;
            match (n, &f.value) {
                (2, Value::Bytes { data, .. }) => p.name = Some(text(data, M, n)?),
                (3, Value::Varint { value, .. }) => p.hash = Some(*value as u32),
                (4, Value::Bytes { data, .. }) => p.date = Some(text(data, M, n)?),
                (5, Value::Fixed32(b)) => p.volume = Some(f32::from_le_bytes(*b)),
                (6, Value::Fixed32(b)) => p.pan = Some(f32::from_le_bytes(*b)),
                (7, Value::Varint { value, .. }) => p.default_scene = Some(*value as u32),
                (8, Value::Bytes { data, .. }) => p.author_name = Some(text(data, M, n)?),
                (9, Value::Bytes { data, .. }) => p.author_id = Some(text(data, M, n)?),
                (10, Value::Varint { value, .. }) => p.tempo = Some(*value as u32),
                (11, Value::Message { body, .. }) => children(&mut p.chains, body)?,
                (12, Value::Bytes { data, .. }) => p.tags.push(text(data, M, n)?),
                (13, Value::Message { body, .. }) => {
                    children(&mut p.legacy_stomp_mode_stomp_data, body)?;
                }
                (14, v @ (Value::Varint { .. } | Value::Bytes { .. })) => {
                    uints(&mut p.scene_tempo, v, M, n)?;
                }
                (15, Value::Bytes { data, .. }) => p.scene_labels.push(text(data, M, n)?),
                (16, Value::Message { body, .. }) => children(&mut p.midi_messages, body)?,
                (17, Value::Message { body, .. }) => {
                    children(&mut p.midi_messages_general, body)?;
                }
                (18, Value::Message { body, .. }) => children(&mut p.bypass, body)?,
                (19, Value::Message { body, .. }) => children(&mut p.tempo_program_data, body)?,
                (20, v @ (Value::Varint { .. } | Value::Bytes { .. })) => {
                    uints(&mut p.layout_code_1, v, M, n)?;
                }
                (21, v @ (Value::Varint { .. } | Value::Bytes { .. })) => {
                    uints(&mut p.layout_code_2, v, M, n)?;
                }
                (22, Value::Bytes { data, .. }) => p.created_version.push(text(data, M, n)?),
                (23, Value::Bytes { data, .. }) => p.modified_version.push(text(data, M, n)?),
                (24, Value::Bytes { data, .. }) => {
                    p.oldest_compatible_version.push(text(data, M, n)?);
                }
                (25, Value::Bytes { data, .. }) => p.cloud_id.push(text(data, M, n)?),
                (26, Value::Bytes { data, .. }) => p.description.push(text(data, M, n)?),
                (27, Value::Message { body, .. }) => {
                    children(&mut p.stomp_mode_assignments, body)?;
                }
                (28, Value::Message { body, .. }) => {
                    children(&mut p.model_update_notifications, body)?;
                }
                (29, v @ (Value::Varint { .. } | Value::Bytes { .. })) => {
                    uints(&mut p.recompile_code_1, v, M, n)?;
                }
                (30, v @ (Value::Varint { .. } | Value::Bytes { .. })) => {
                    uints(&mut p.recompile_code_2, v, M, n)?;
                }
                (31, v @ (Value::Varint { .. } | Value::Bytes { .. })) => {
                    uints(&mut p.scene_colors, v, M, n)?;
                }
                (32, Value::Message { body, .. }) => {
                    let (key, value) = string_entry(body, "BinaryPreset.StompLabelsEntry")?;
                    p.stomp_labels.insert(key, value);
                }
                (33, Value::Message { body, .. }) => {
                    children(&mut p.midi_messages_general_v2, body)?;
                }
                (34, Value::Message { body, .. }) => {
                    let (key, value) = string_entry(body, "BinaryPreset.SingleStompLabelsEntry")?;
                    p.single_stomp_labels.insert(key, value);
                }
                (35, Value::Message { body, .. }) => {
                    let (key, value) = entry(body);
                    let value = matches!(value, Some(Value::Varint { value, .. }) if *value != 0);
                    p.stomp_is_momentary.insert(key, value);
                }
                (36, Value::Varint { value, .. }) => p.side_chain_follow_exists = Some(*value != 0),
                (37, Value::Varint { value, .. }) => p.zsk_7182_applied = Some(*value != 0),
                _ => p.unknown.push(f),
            }
        }
        Ok(p)
    }
}

impl Decode for Chain {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut c = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Varint { value, .. }) => c.in_portid = Some(*value as u32),
                (2, Value::Varint { value, .. }) => c.out_portid = Some(*value as u32),
                (5, Value::Message { body, .. }) => children(&mut c.models, body)?,
                (6, Value::Message { body, .. }) => children(&mut c.split_control_points, body)?,
                (7, Value::Message { body, .. }) => children(&mut c.splitter, body)?,
                (8, Value::Message { body, .. }) => children(&mut c.mixer, body)?,
                (9, Value::Message { body, .. }) => children(&mut c.output_control, body)?,
                (10, Value::Message { body, .. }) => children(&mut c.split_bypass, body)?,
                (11, Value::Message { body, .. }) => children(&mut c.mix_bypass, body)?,
                (12, Value::Message { body, .. }) => children(&mut c.combined_splitter, body)?,
                (13, Value::Message { body, .. }) => children(&mut c.input_control, body)?,
                (14, Value::Varint { value, .. }) => c.row = Some(*value as u32),
                _ => c.unknown.push(f),
            }
        }
        Ok(c)
    }
}

impl Decode for Model {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut model = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Varint { value, .. }) => model.hash = Some(*value as u32),
                (2, Value::Message { body, .. }) => children(&mut model.params, body)?,
                (3, Value::Message { body, .. }) => children(&mut model.bypass_expression, body)?,
                (4, Value::Message { body, .. }) => {
                    children(&mut model.expression_bypass_info, body)?;
                }
                (5, Value::Varint { value, .. }) => model.column = Some(*value as u32),
                (6, Value::Varint { value, .. }) => model.notify_flag = Some(*value != 0),
                (7, Value::Varint { value, .. }) => model.sidechain_sink_flag = Some(*value != 0),
                (8, Value::Varint { value, .. }) => {
                    model.sidechain_source_flag = Some(*value != 0);
                }
                _ => model.unknown.push(f),
            }
        }
        Ok(model)
    }
}

impl Decode for Param {
    fn decode_message(m: &Message) -> Result<Self> {
        const M: &str = "Param";
        let mut p = Self::default();
        for f in &m.fields {
            let n = f.number;
            match (n, &f.value) {
                (1, Value::Varint { value, .. }) => p.expression = Some(*value as i32),
                (2, Value::Fixed32(b)) => p.expression_min = Some(f32::from_le_bytes(*b)),
                (3, Value::Fixed32(b)) => p.expression_max = Some(f32::from_le_bytes(*b)),
                (4, Value::Varint { value, .. }) => p.scene_mode = Some(*value != 0),
                (5, Value::Message { body, .. }) => children(&mut p.values, body)?,
                (6, Value::Varint { value, .. }) => p.index = Some(*value as u32),
                (7, Value::Varint { value, .. }) => p.is_led = Some(*value != 0),
                (8, Value::Bytes { data, .. }) => p.dynamic_steps.push(text(data, M, n)?),
                (9, Value::Bytes { data, .. }) => p.dynamic_icons.push(text(data, M, n)?),
                (10, Value::Bytes { data, .. }) => p.dynamic_metadata.push(text(data, M, n)?),
                _ => p.unknown.push(f),
            }
        }
        Ok(p)
    }
}

impl Decode for SceneValue {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut v = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Varint { value, .. }) => v.value = Some(ParamValue::Int(*value as i32)),
                (2, Value::Fixed32(b)) => v.value = Some(ParamValue::Float(f32::from_le_bytes(*b))),
                (3, Value::Bytes { data, .. }) => {
                    v.value = Some(ParamValue::String(text(data, "ParamValue", 3)?));
                }
                _ => v.unknown.push(f),
            }
        }
        Ok(v)
    }
}

impl Decode for SplitControlPoints {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut s = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Varint { value, .. }) => s.split = *value as i32,
                (2, Value::Varint { value, .. }) => s.mix = *value as i32,
                _ => s.unknown.push(f),
            }
        }
        Ok(s)
    }
}

impl Decode for BypassRow {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut b = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Message { body, .. }) => children(&mut b.col_bypass, body)?,
                (2, Value::Varint { value, .. }) => b.row = Some(*value as u32),
                _ => b.unknown.push(f),
            }
        }
        Ok(b)
    }
}

impl Decode for ColBypass {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut c = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Message { body, .. }) => children(&mut c.scene_bypass, body)?,
                (2, Value::Varint { value, .. }) => c.column = Some(*value as u32),
                (3, Value::Varint { value, .. }) => c.scene_mode = Some(*value != 0),
                _ => c.unknown.push(f),
            }
        }
        Ok(c)
    }
}

impl Decode for SceneBypass {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut s = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Varint { value, .. }) => s.bypass = *value != 0,
                _ => s.unknown.push(f),
            }
        }
        Ok(s)
    }
}

impl Decode for GridRef {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut g = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Varint { value, .. }) => g.row = *value as u32,
                (2, Value::Varint { value, .. }) => g.column = *value as u32,
                _ => g.unknown.push(f),
            }
        }
        Ok(g)
    }
}

impl Decode for StompAssignment {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut s = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Varint { value, .. }) => s.row = *value as u32,
                (2, Value::Varint { value, .. }) => s.column = *value as u32,
                (3, Value::Varint { value, .. }) => s.stomp_index = *value as u32,
                (4, Value::Varint { value, .. }) => {
                    s.role = Some(StompRole::from_raw(*value as i32));
                }
                _ => s.unknown.push(f),
            }
        }
        Ok(s)
    }
}

impl Decode for MidiMessage {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut x = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Varint { value, .. }) => x.kind = *value as u32,
                (2, Value::Varint { value, .. }) => x.channel = *value as u32,
                (3, Value::Varint { value, .. }) => x.param1 = *value as u32,
                (4, Value::Varint { value, .. }) => x.param2 = *value as u32,
                (5, Value::Varint { value, .. }) => x.param3 = *value as u32,
                _ => x.unknown.push(f),
            }
        }
        Ok(x)
    }
}

impl Decode for Expression {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut e = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Varint { value, .. }) => e.expression = *value as i32,
                (2, Value::Fixed32(b)) => e.expression_min = f32::from_le_bytes(*b),
                (3, Value::Fixed32(b)) => e.expression_max = f32::from_le_bytes(*b),
                _ => e.unknown.push(f),
            }
        }
        Ok(e)
    }
}

impl Decode for ExpressionBypassInfo {
    fn decode_message(m: &Message) -> Result<Self> {
        let mut e = Self::default();
        for f in &m.fields {
            match (f.number, &f.value) {
                (1, Value::Varint { value, .. }) => e.kind = *value as u32,
                (2, Value::Varint { value, .. }) => e.invert = *value != 0,
                (3, Value::Varint { value, .. }) => e.delay_ms = *value as u32,
                (4, Value::Varint { value, .. }) => e.latch_emulation = *value != 0,
                _ => e.unknown.push(f),
            }
        }
        Ok(e)
    }
}

// ---------------------------------------------------------------------------
// Stock serializer layout.

trait Encode {
    fn encode(&self, out: &mut Vec<u8>);
}

fn key(out: &mut Vec<u8>, number: u32, wire: u8) {
    out.extend_from_slice(&wire::encode_varint(
        u64::from(number) << 3 | u64::from(wire),
    ));
}

fn put_u32(out: &mut Vec<u8>, number: u32, value: u32) {
    key(out, number, 0);
    out.extend_from_slice(&wire::encode_varint(u64::from(value)));
}

/// Negative int32 values sign-extend to ten bytes, as stock writes -1.
fn put_i32(out: &mut Vec<u8>, number: u32, value: i32) {
    key(out, number, 0);
    out.extend_from_slice(&wire::encode_varint(i64::from(value) as u64));
}

fn put_f32(out: &mut Vec<u8>, number: u32, value: f32) {
    key(out, number, 5);
    out.extend(value.to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, number: u32, data: &[u8]) {
    key(out, number, 2);
    out.extend_from_slice(&wire::encode_varint(data.len() as u64));
    out.extend_from_slice(data);
}

fn opt_u32(out: &mut Vec<u8>, number: u32, value: Option<u32>) {
    if let Some(value) = value {
        put_u32(out, number, value);
    }
}

fn opt_i32(out: &mut Vec<u8>, number: u32, value: Option<i32>) {
    if let Some(value) = value {
        put_i32(out, number, value);
    }
}

fn opt_bool(out: &mut Vec<u8>, number: u32, value: Option<bool>) {
    if let Some(value) = value {
        put_u32(out, number, value.into());
    }
}

fn opt_f32(out: &mut Vec<u8>, number: u32, value: Option<f32>) {
    if let Some(value) = value {
        put_f32(out, number, value);
    }
}

fn opt_str(out: &mut Vec<u8>, number: u32, value: Option<&String>) {
    if let Some(value) = value {
        put_bytes(out, number, value.as_bytes());
    }
}

fn nz_u32(out: &mut Vec<u8>, number: u32, value: u32) {
    if value != 0 {
        put_u32(out, number, value);
    }
}

fn nz_i32(out: &mut Vec<u8>, number: u32, value: i32) {
    if value != 0 {
        put_i32(out, number, value);
    }
}

fn nz_bool(out: &mut Vec<u8>, number: u32, value: bool) {
    if value {
        put_u32(out, number, 1);
    }
}

/// Proto3 implicit floats: protobuf 3.18 skips a value when
/// `x <= 0 && x >= 0` (`0x008aa928`, VA 0x8aa9d4), so it writes NaN but
/// neither 0.0 nor -0.0.
fn nz_f32(out: &mut Vec<u8>, number: u32, value: f32) {
    if value != 0.0 {
        put_f32(out, number, value);
    }
}

fn strs(out: &mut Vec<u8>, number: u32, values: &[String]) {
    for value in values {
        put_bytes(out, number, value.as_bytes());
    }
}

/// A length-delimited field whose payload `body` writes straight into `out`. Its length goes
/// in front afterwards, so no payload needs a buffer of its own.
fn put_nested(out: &mut Vec<u8>, number: u32, body: impl FnOnce(&mut Vec<u8>)) {
    key(out, number, 2);
    let start = out.len();
    body(out);
    let length = wire::encode_varint((out.len() - start) as u64);
    out.splice(start..start, length.iter().copied());
}

fn packed(out: &mut Vec<u8>, number: u32, values: &[u32]) {
    if values.is_empty() {
        return;
    }
    put_nested(out, number, |out| {
        for &value in values {
            out.extend_from_slice(&wire::encode_varint(u64::from(value)));
        }
    });
}

fn msgs<T: Encode>(out: &mut Vec<u8>, number: u32, values: &[T]) {
    for value in values {
        put_nested(out, number, |out| value.encode(out));
    }
}

fn string_map(out: &mut Vec<u8>, number: u32, map: &MapField<String>) {
    for (key, value) in map.iter() {
        put_nested(out, number, |out| {
            put_u32(out, 1, key);
            put_bytes(out, 2, value.as_bytes());
        });
    }
}

impl Encode for Preset {
    fn encode(&self, out: &mut Vec<u8>) {
        opt_str(out, 2, self.name.as_ref());
        opt_u32(out, 3, self.hash);
        opt_str(out, 4, self.date.as_ref());
        opt_f32(out, 5, self.volume);
        opt_f32(out, 6, self.pan);
        opt_u32(out, 7, self.default_scene);
        opt_str(out, 8, self.author_name.as_ref());
        opt_str(out, 9, self.author_id.as_ref());
        opt_u32(out, 10, self.tempo);
        msgs(out, 11, &self.chains);
        strs(out, 12, &self.tags);
        msgs(out, 13, &self.legacy_stomp_mode_stomp_data);
        packed(out, 14, &self.scene_tempo);
        strs(out, 15, &self.scene_labels);
        msgs(out, 16, &self.midi_messages);
        msgs(out, 17, &self.midi_messages_general);
        msgs(out, 18, &self.bypass);
        msgs(out, 19, &self.tempo_program_data);
        packed(out, 20, &self.layout_code_1);
        packed(out, 21, &self.layout_code_2);
        strs(out, 22, &self.created_version);
        strs(out, 23, &self.modified_version);
        strs(out, 24, &self.oldest_compatible_version);
        strs(out, 25, &self.cloud_id);
        strs(out, 26, &self.description);
        msgs(out, 27, &self.stomp_mode_assignments);
        msgs(out, 28, &self.model_update_notifications);
        packed(out, 29, &self.recompile_code_1);
        packed(out, 30, &self.recompile_code_2);
        packed(out, 31, &self.scene_colors);
        string_map(out, 32, &self.stomp_labels);
        msgs(out, 33, &self.midi_messages_general_v2);
        string_map(out, 34, &self.single_stomp_labels);
        for (key, value) in self.stomp_is_momentary.iter() {
            put_nested(out, 35, |out| {
                put_u32(out, 1, key);
                put_u32(out, 2, (*value).into());
            });
        }
        opt_bool(out, 36, self.side_chain_follow_exists);
        opt_bool(out, 37, self.zsk_7182_applied);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for Chain {
    fn encode(&self, out: &mut Vec<u8>) {
        opt_u32(out, 1, self.in_portid);
        opt_u32(out, 2, self.out_portid);
        msgs(out, 5, &self.models);
        msgs(out, 6, &self.split_control_points);
        msgs(out, 7, &self.splitter);
        msgs(out, 8, &self.mixer);
        msgs(out, 9, &self.output_control);
        msgs(out, 10, &self.split_bypass);
        msgs(out, 11, &self.mix_bypass);
        msgs(out, 12, &self.combined_splitter);
        msgs(out, 13, &self.input_control);
        opt_u32(out, 14, self.row);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for Model {
    fn encode(&self, out: &mut Vec<u8>) {
        opt_u32(out, 1, self.hash);
        msgs(out, 2, &self.params);
        msgs(out, 3, &self.bypass_expression);
        msgs(out, 4, &self.expression_bypass_info);
        opt_u32(out, 5, self.column);
        opt_bool(out, 6, self.notify_flag);
        opt_bool(out, 7, self.sidechain_sink_flag);
        opt_bool(out, 8, self.sidechain_source_flag);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for Param {
    fn encode(&self, out: &mut Vec<u8>) {
        opt_i32(out, 1, self.expression);
        opt_f32(out, 2, self.expression_min);
        opt_f32(out, 3, self.expression_max);
        opt_bool(out, 4, self.scene_mode);
        msgs(out, 5, &self.values);
        opt_u32(out, 6, self.index);
        opt_bool(out, 7, self.is_led);
        strs(out, 8, &self.dynamic_steps);
        strs(out, 9, &self.dynamic_icons);
        strs(out, 10, &self.dynamic_metadata);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for SceneValue {
    fn encode(&self, out: &mut Vec<u8>) {
        match &self.value {
            Some(ParamValue::Int(value)) => put_i32(out, 1, *value),
            Some(ParamValue::Float(value)) => put_f32(out, 2, *value),
            Some(ParamValue::String(value)) => put_bytes(out, 3, value.as_bytes()),
            None => {}
        }
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for SplitControlPoints {
    fn encode(&self, out: &mut Vec<u8>) {
        nz_i32(out, 1, self.split);
        nz_i32(out, 2, self.mix);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for BypassRow {
    fn encode(&self, out: &mut Vec<u8>) {
        msgs(out, 1, &self.col_bypass);
        opt_u32(out, 2, self.row);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for ColBypass {
    fn encode(&self, out: &mut Vec<u8>) {
        msgs(out, 1, &self.scene_bypass);
        opt_u32(out, 2, self.column);
        opt_bool(out, 3, self.scene_mode);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for SceneBypass {
    fn encode(&self, out: &mut Vec<u8>) {
        nz_bool(out, 1, self.bypass);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for GridRef {
    fn encode(&self, out: &mut Vec<u8>) {
        nz_u32(out, 1, self.row);
        nz_u32(out, 2, self.column);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for StompAssignment {
    fn encode(&self, out: &mut Vec<u8>) {
        nz_u32(out, 1, self.row);
        nz_u32(out, 2, self.column);
        nz_u32(out, 3, self.stomp_index);
        opt_i32(out, 4, self.role.map(StompRole::raw));
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for MidiMessage {
    fn encode(&self, out: &mut Vec<u8>) {
        nz_u32(out, 1, self.kind);
        nz_u32(out, 2, self.channel);
        nz_u32(out, 3, self.param1);
        nz_u32(out, 4, self.param2);
        nz_u32(out, 5, self.param3);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for Expression {
    fn encode(&self, out: &mut Vec<u8>) {
        nz_i32(out, 1, self.expression);
        nz_f32(out, 2, self.expression_min);
        nz_f32(out, 3, self.expression_max);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}

impl Encode for ExpressionBypassInfo {
    fn encode(&self, out: &mut Vec<u8>) {
        nz_u32(out, 1, self.kind);
        nz_bool(out, 2, self.invert);
        nz_u32(out, 3, self.delay_ms);
        nz_bool(out, 4, self.latch_emulation);
        out.extend_from_slice(self.unknown.as_bytes());
    }
}
