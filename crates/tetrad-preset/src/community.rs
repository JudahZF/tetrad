//! Community blocks in stock presets (owned format, (O)).
//!
//! A community block has a local model ID that stock CorOS does not know. Before Tetrad writes
//! a preset, [`stash`] puts a stock placeholder model in each community slot and records the
//! original model in [`TETRAD_BLOCKS_FIELD`]. Stock loads and plays the placeholder and keeps
//! the field when it saves. After Tetrad reads a preset, [`restore`] puts the community blocks
//! back.
//!
//! The field is UTF-8 JSON:
//! `{"version":1,"blocks":[{"row":0,"column":2,"placeholder":10002,"key":"…","model":"…"}]}`.
//! `model` is the standard padded base64 of the encoded community `Model` message as Tetrad had
//! it (own hash, params, scene values, expressions and unknown fields).
//!
//! Positions are positional, as in [`crate::Slot`] and the stock loader: `row` is the position
//! in `Preset.chains` and `column` is the position in `Chain.models`. Stored `Chain.row` and
//! `Model.column` fields are not used. Bypass records, stomp assignments and other per-slot data
//! stay in place; only the slot's model changes.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

pub use crate::model::TETRAD_BLOCKS_FIELD;
use crate::{
    Error, Result,
    model::{Model, Preset},
};

const VERSION: u32 = 1;

/// What [`stash`] writes for one community model ID.
#[derive(Debug, Clone, PartialEq)]
pub struct Stash {
    /// Community block key that [`restore`] resolves to a local model ID.
    pub key: String,
    /// Complete stock model with default params. [`stash`] sets its stored `column` to the slot
    /// position when the field is present (the [`Preset::replace_model`] rule).
    pub placeholder: Model,
}

/// A placeholder whose community block is not installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub row: usize,
    pub column: usize,
    pub key: String,
}

#[derive(Serialize, Deserialize)]
struct Records {
    version: u32,
    blocks: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    row: usize,
    column: usize,
    placeholder: u32,
    key: String,
    model: String,
}

#[derive(Deserialize)]
struct Header {
    version: u32,
}

fn invalid(reason: impl std::fmt::Display) -> Error {
    Error::CommunityRecords(reason.to_string())
}

/// The stored records, or `None` when the field is absent or has an unknown version.
fn read(preset: &Preset) -> Result<Option<Vec<Record>>> {
    let Some(bytes) = preset.unknown.bytes_field(TETRAD_BLOCKS_FIELD) else {
        return Ok(None);
    };
    let header: Header = serde_json::from_slice(&bytes).map_err(invalid)?;
    if header.version != VERSION {
        return Ok(None);
    }
    let records: Records = serde_json::from_slice(&bytes).map_err(invalid)?;
    Ok(Some(records.blocks))
}

/// True when the record's slot still holds its placeholder model.
fn holds_placeholder(preset: &Preset, record: &Record) -> bool {
    preset
        .chains
        .get(record.row)
        .and_then(|chain| chain.models.get(record.column))
        .is_some_and(|model| model.hash == Some(record.placeholder))
}

/// Replaces each community block with its placeholder and writes the records field.
///
/// `community` maps a slot's model ID to a [`Stash`], or `None` for a stock model. A previous
/// record stays when its slot still holds its placeholder and no new record replaces it: that
/// block is not installed now and [`restore`] can put it back later. Other previous records go.
/// (O) Previous records that cannot be read (malformed JSON or an unknown version) are dropped.
/// The field is removed when no records remain and is not rewritten when it does not change.
/// Nothing changes on an error.
pub fn stash(preset: &mut Preset, community: impl Fn(u32) -> Option<Stash>) -> Result<()> {
    let mut stashes = Vec::new();
    for (row, chain) in preset.chains.iter().enumerate() {
        for (column, model) in chain.models.iter().enumerate() {
            let Some(stash) = model.hash.filter(|&id| id != 0).and_then(&community) else {
                continue;
            };
            if stash.placeholder.is_empty() {
                return Err(Error::Unsupported(
                    "a community placeholder needs a nonzero model ID",
                ));
            }
            stashes.push((row, column, stash));
        }
    }

    let mut records = read(preset).ok().flatten().unwrap_or_default();
    records.retain(|record| {
        holds_placeholder(preset, record)
            && !stashes
                .iter()
                .any(|&(row, column, _)| (row, column) == (record.row, record.column))
    });
    for (row, column, Stash { key, placeholder }) in stashes {
        let slot = &mut preset.chains[row].models[column];
        records.push(Record {
            row,
            column,
            placeholder: placeholder.hash.unwrap_or_default(),
            key,
            model: STANDARD.encode(slot.to_bytes()),
        });
        *slot = Model {
            column: placeholder.column.and(Some(column as u32)),
            ..placeholder
        };
    }
    write(preset, records)
}

/// Writes `records` sorted, one per slot, or removes the field when there are none. An unchanged
/// field is not rewritten.
fn write(preset: &mut Preset, mut records: Vec<Record>) -> Result<()> {
    records.sort_by_key(|record| (record.row, record.column));
    records.dedup_by_key(|record| (record.row, record.column));
    let bytes = if records.is_empty() {
        None
    } else {
        let records = Records {
            version: VERSION,
            blocks: records,
        };
        Some(serde_json::to_vec(&records).map_err(invalid)?)
    };
    if preset.unknown.bytes_field(TETRAD_BLOCKS_FIELD) != bytes {
        preset.unknown.set_bytes_field(TETRAD_BLOCKS_FIELD, bytes)?;
    }
    Ok(())
}

/// Puts the recorded community blocks back and returns the ones that are not installed.
///
/// `resolve` maps a community block key to its local model ID. A record applies only while its
/// slot holds its placeholder; a slot that stock changed keeps the stock block. A resolved record
/// gets back its stored model with the resolved ID as hash. An unresolved record leaves the
/// placeholder and is reported as [`Missing`]. Only those unresolved records stay in the field:
/// a restored block is in its slot again, and [`stash`] writes its record anew; a record whose
/// slot no longer holds the placeholder is stale, so that a later edit that puts the placeholder
/// model in the slot cannot bring the old block back. An unknown version is ignored and left
/// unchanged. Malformed records give [`Error::CommunityRecords`] and change nothing.
pub fn restore(preset: &mut Preset, resolve: impl Fn(&str) -> Option<u32>) -> Result<Vec<Missing>> {
    let Some(records) = read(preset)? else {
        return Ok(Vec::new());
    };
    // Decode every record first, so that an error changes nothing.
    let models = records
        .iter()
        .map(|record| {
            let bytes = STANDARD.decode(&record.model).map_err(|error| {
                invalid(format!("({}, {}): {error}", record.row, record.column))
            })?;
            Model::decode(&bytes)
                .map_err(|error| invalid(format!("({}, {}): {error}", record.row, record.column)))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut missing = Vec::new();
    let mut kept = Vec::new();
    for (record, model) in records.into_iter().zip(models) {
        if !holds_placeholder(preset, &record) {
            continue;
        }
        match resolve(&record.key).filter(|&id| id != 0) {
            Some(id) => {
                preset.chains[record.row].models[record.column] = Model {
                    hash: Some(id),
                    ..model
                };
            }
            None => {
                missing.push(Missing {
                    row: record.row,
                    column: record.column,
                    key: record.key.clone(),
                });
                kept.push(record);
            }
        }
    }
    write(preset, kept)?;
    Ok(missing)
}
