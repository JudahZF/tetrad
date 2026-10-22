//! Lossless CorOS 4.1.0 preset data and targeted offline edits.
//!
//! Wire bytes remain authoritative: no-op exports preserve unknown fields, order,
//! duplicates, explicit defaults, groups and packed encodings. Typed views use the
//! last scalar/oneof occurrence and positional repeated messages. Validation is a
//! schema/data check, not a graph compiler or device compatibility guarantee.
//!
//! [`Preset`] is the typed editable model. It decodes and saves with the stock
//! protobuf semantics and keeps unknown fields. [`PresetDocument::modify`] runs
//! typed edits as one transaction and keeps the original bytes when nothing changes.
#![forbid(unsafe_code)]

pub mod community;
mod create;
mod diagnostic;
mod edit;
mod migrate;
pub mod model;
pub mod schema;
mod sidechain;
mod summary;
mod wire;

pub use create::SaveStamp;
pub use diagnostic::{Diagnostic, Severity};
pub use edit::{COLUMNS, MAX_STOMP_INDEX, MidiList, ROWS, SCENES, Slot};
pub use migrate::{
    CatalogModel, CatalogParam, INPUT_CONTROL_ID, MIDI_GENERAL_GROUPS, MIDI_GENERAL_SLOTS,
    MIDI_MESSAGES, MIXER_ID, MigrationReport, ModelCatalog, OUTPUT_CONTROL_ID, TEMPO_MODEL_ID,
    TEMPO_RANGE, UnknownBlock, parse_version,
};
pub use model::{
    BypassRow, Chain, ColBypass, Expression, ExpressionBypassInfo, GridRef, MapField, MidiMessage,
    Model, Param, Preset, SceneBypass, SceneValue, SplitControlPoints, StompAssignment, StompRole,
    Unknown,
};
use serde::Serialize;
pub use summary::{
    BypassSummary, ChainSummary, ColBypassSummary, ModelSummary, ParamSummary, PresetSummary,
    SceneBypassSummary, SceneSummary, SplitControlPointsSummary, WirePresence,
};
pub use tetrad_core::{ImportVersions, SplitterType, Version};
use wire::{Field, Message, Value};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("truncated protobuf at byte {offset}")]
    Truncated { offset: usize },
    #[error("varint overflow at byte {offset}")]
    VarintOverflow { offset: usize },
    #[error("length overflow at byte {offset}")]
    LengthOverflow { offset: usize },
    #[error("invalid protobuf field number at byte {offset}")]
    InvalidFieldNumber { offset: usize },
    #[error("invalid wire type {wire} at byte {offset}")]
    InvalidWire { offset: usize, wire: u8 },
    #[error("unmatched or unterminated protobuf group at byte {offset}")]
    InvalidGroup { offset: usize },
    #[error("protobuf {kind} limit exceeded ({limit})")]
    Limit { kind: &'static str, limit: usize },
    #[error("invalid known field {message}.{field}: {reason}")]
    InvalidKnownField {
        message: &'static str,
        field: u32,
        reason: &'static str,
    },
    #[error("{kind} index {index} is outside the stored list of {len} entries")]
    Index {
        kind: &'static str,
        index: usize,
        len: usize,
    },
    #[error("parameter float must be finite")]
    NonFinite,
    #[error("preset does not have the stock grid layout: {0}")]
    Layout(&'static str),
    #[error("grid slot ({row}, {column}) is occupied")]
    SlotOccupied { row: usize, column: usize },
    #[error("grid slot ({row}, {column}) is empty")]
    SlotEmpty { row: usize, column: usize },
    #[error("unsupported preset operation: {0}")]
    Unsupported(&'static str),
    #[error("model ID {0} is not in the catalog")]
    UnknownModel(u32),
    #[error("invalid Tetrad community block records: {0}")]
    CommunityRecords(String),
    #[error(transparent)]
    Core(#[from] tetrad_core::Error),
}

/// Limits count wire fields in known messages and unknown groups, plus
/// packed scalar elements (to bound typed view allocations).
/// Opaque unknown length-delimited payloads are bounded by `max_bytes`.
/// Root depth is zero; effective nesting is capped at 128 for stack safety.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_fields: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_depth: 32,
            max_fields: 100_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ParamValue {
    Int(i32),
    Float(f32),
    String(String),
}

#[derive(Debug, Clone)]
pub struct PresetDocument {
    root: Message,
    limits: Limits,
}

impl PresetDocument {
    pub fn parse(data: &[u8], mut limits: Limits) -> Result<Self> {
        limits.max_depth = limits.max_depth.min(128);
        Ok(Self {
            root: wire::parse(data, limits)?,
            limits,
        })
    }

    pub fn summary(&self) -> Result<PresetSummary> {
        summary::summarize(&self.root)
    }

    /// Schema and recovered size/range checks. Does not compile a signal graph.
    pub fn validate(&self) -> Result<Vec<Diagnostic>> {
        Ok(diagnostic::validate(&self.root))
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.root.size());
        self.root.write(&mut bytes);
        bytes
    }

    pub(crate) fn root(&self) -> &Message {
        &self.root
    }

    /// Runs typed edits as one transaction. When the typed result differs from
    /// the decoded original, the document becomes the stock serializer layout of
    /// the result; otherwise the original bytes stay exactly as they were.
    /// Errors (including limit failures) leave the document unchanged.
    pub fn modify<T>(&mut self, edit: impl FnOnce(&mut Preset) -> Result<T>) -> Result<T> {
        let original = Preset::try_from(&*self)?;
        let mut preset = original.clone();
        let output = edit(&mut preset)?;
        let bytes = preset.to_bytes();
        if bytes != original.to_bytes() {
            *self = Self::parse(&bytes, self.limits)?;
        }
        Ok(output)
    }

    /// Updates the last name occurrence, preserving earlier duplicate names.
    /// An absent name is appended with explicit presence.
    pub fn rename(&mut self, name: &str) -> Result<()> {
        summary::check_field(&self.root, 2)?;
        let position = self.root.positions(2).last();
        if summary::string(&self.root, 2)?.as_deref() == Some(name) {
            return Ok(());
        }
        let mut field = wire::bytes_field(2, name.as_bytes().to_vec());
        if let Some(i) = position {
            preserve_tag(&self.root.fields[i], &mut field);
        }
        self.edit(&[position.unwrap_or(self.root.fields.len())], field)
    }

    /// Edits an existing positional scene label; does not create missing scenes.
    pub fn set_scene_label(&mut self, index: usize, label: &str) -> Result<()> {
        summary::check_field(&self.root, 15)?;
        let positions: Vec<_> = self.root.positions(15).collect();
        let position = *positions.get(index).ok_or(Error::Index {
            kind: "scene label",
            index,
            len: positions.len(),
        })?;
        let old = &self.root.fields[position];
        if let Value::Bytes { data, .. } = &old.value
            && data == label.as_bytes()
        {
            return Ok(());
        }
        let mut field = wire::bytes_field(15, label.as_bytes().to_vec());
        preserve_tag(old, &mut field);
        self.edit(&[position], field)
    }

    /// Edits an existing unpacked field-31 color, preserving its tag and explicit
    /// zero. Equal decoded values keep the exact original varint. Owned restrictive
    /// policy: any packed field 31 refuses this editor, including mixed layouts,
    /// so a logical color index cannot be mistaken for a wire occurrence index.
    /// Missing entries and failed limit checks leave the document unchanged.
    pub fn set_scene_color(&mut self, index: usize, raw_color: u32) -> Result<()> {
        summary::check_field(&self.root, 31)?;
        let positions: Vec<_> = self.root.positions(31).collect();
        if positions
            .iter()
            .any(|&position| self.root.fields[position].value.wire() == 2)
        {
            return Err(summary::invalid(
                &self.root,
                31,
                "packed scene colors are unsupported by the positional color editor",
            ));
        }
        let position = *positions.get(index).ok_or(Error::Index {
            kind: "scene color",
            index,
            len: positions.len(),
        })?;
        let old = &self.root.fields[position];
        let Value::Varint { value, .. } = &old.value else {
            return Err(summary::invalid(&self.root, 31, "expected uint32"));
        };
        let previous = u32::try_from(*value)
            .map_err(|_| summary::invalid(&self.root, 31, "uint32 overflow"))?;
        if previous == raw_color {
            return Ok(());
        }
        let mut field = Field {
            number: 31,
            tag: wire::encode_varint(31 << 3),
            value: Value::Varint {
                raw: wire::encode_varint(u64::from(raw_color)),
                value: u64::from(raw_color),
            },
        };
        preserve_tag(old, &mut field);
        self.edit(&[position], field)
    }

    /// Edits Chain.models → Model.params → Param.param_values by stored position.
    /// Replaces the final known oneof occurrence (protobuf last-one-wins).
    /// Earlier oneof alternatives/duplicates and all unknown fields survive.
    /// An empty ParamValue gets a new value, but missing list entries are errors.
    pub fn set_parameter(
        &mut self,
        chain: usize,
        model: usize,
        param: usize,
        scene: usize,
        value: ParamValue,
    ) -> Result<()> {
        if matches!(value, ParamValue::Float(v) if !v.is_finite()) {
            return Err(Error::NonFinite);
        }
        let mut path = Vec::with_capacity(5);
        let mut current = &self.root;
        for (number, index, kind) in [
            (11, chain, "chain"),
            (5, model, "model"),
            (2, param, "parameter"),
            (5, scene, "parameter scene"),
        ] {
            summary::check_field(current, number)?;
            let positions: Vec<_> = current.positions(number).collect();
            let position = *positions.get(index).ok_or(Error::Index {
                kind,
                index,
                len: positions.len(),
            })?;
            path.push(position);
            current = summary::child(&current.fields[position], current)?;
        }
        let previous = summary::parameter_value(current)?;
        if match (&previous, &value) {
            (Some(ParamValue::Float(a)), ParamValue::Float(b)) => a.to_bits() == b.to_bits(),
            (Some(a), b) => a == b,
            _ => false,
        } {
            return Ok(());
        }
        let position = current
            .fields
            .iter()
            .rposition(|f| (1..=3).contains(&f.number));
        let mut field = match value {
            ParamValue::Int(value) => Field {
                number: 1,
                tag: vec![8],
                value: Value::Varint {
                    raw: wire::encode_varint(i64::from(value) as u64),
                    value: i64::from(value) as u64,
                },
            },
            ParamValue::Float(value) => Field {
                number: 2,
                tag: vec![21],
                value: Value::Fixed32(value.to_le_bytes()),
            },
            ParamValue::String(value) => wire::bytes_field(3, value.into_bytes()),
        };
        if let Some(i) = position {
            preserve_tag(&current.fields[i], &mut field);
        }
        path.push(position.unwrap_or(current.fields.len()));
        self.edit(&path, field)
    }

    fn edit(&mut self, path: &[usize], field: Field) -> Result<()> {
        // Account for every changed ancestor prefix before copying the tree.
        let limit_error = || Error::Limit {
            kind: "bytes",
            limit: self.limits.max_bytes,
        };
        let mut ancestors = Vec::with_capacity(path.len() - 1);
        let mut parent = &self.root;
        for &position in &path[..path.len() - 1] {
            let ancestor = &parent.fields[position];
            let child = summary::child(ancestor, parent)?;
            if let Value::Message { length, .. } = &ancestor.value {
                ancestors.push((
                    parent.size(),
                    ancestor.tag.len(),
                    length.len(),
                    child.size(),
                ));
            }
            parent = child;
        }
        let removed = parent
            .fields
            .get(path[path.len() - 1])
            .map_or(0, |f| f.tag.len() + f.value.size());
        let added = field
            .tag
            .len()
            .checked_add(field.value.size())
            .ok_or_else(limit_error)?;
        let mut size = parent
            .size()
            .checked_sub(removed)
            .and_then(|size| size.checked_add(added))
            .ok_or_else(limit_error)?;
        for (parent_size, tag_size, prefix_size, child_size) in ancestors.into_iter().rev() {
            let new_prefix_size = if size == child_size {
                prefix_size
            } else {
                wire::encode_varint(size as u64).len()
            };
            let new_field_size = tag_size
                .checked_add(new_prefix_size)
                .and_then(|prefix| prefix.checked_add(size))
                .ok_or_else(limit_error)?;
            size = parent_size
                .checked_sub(tag_size + prefix_size + child_size)
                .and_then(|size| size.checked_add(new_field_size))
                .ok_or_else(limit_error)?;
        }
        if size > self.limits.max_bytes {
            return Err(Error::Limit {
                kind: "bytes",
                limit: self.limits.max_bytes,
            });
        }
        let mut candidate = self.root.clone();
        candidate.replace(path, field);
        if candidate.size() > self.limits.max_bytes {
            return Err(Error::Limit {
                kind: "bytes",
                limit: self.limits.max_bytes,
            });
        }
        let mut bytes = Vec::with_capacity(candidate.size());
        candidate.write(&mut bytes);
        self.root = wire::parse(&bytes, self.limits)?;
        Ok(())
    }
}

fn preserve_tag(old: &Field, new: &mut Field) {
    if old.number == new.number && old.value.wire() == new.value.wire() {
        new.tag.clone_from(&old.tag);
    }
    // Preserve an existing overlong length prefix when the payload size is equal.
    if let (
        Value::Bytes {
            length: old_length,
            data: old_data,
        },
        Value::Bytes { length, data },
    ) = (&old.value, &mut new.value)
        && old.number == new.number
        && old_data.len() == data.len()
    {
        length.clone_from(old_length);
    }
}
