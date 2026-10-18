use crate::{
    Error, ParamValue, Result,
    schema::Kind,
    wire::{Field, Message, Value},
};
use serde::Serialize;
use std::collections::BTreeMap;

/// Direct wire occurrences, including explicit default values and unknown fields.
#[derive(Debug, Clone, Serialize)]
pub struct WirePresence {
    pub number: u32,
    pub name: Option<&'static str>,
    pub optional: bool,
    pub occurrences: usize,
    pub wire_types: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresetSummary {
    pub name: Option<String>,
    /// Stored identifier only; the library does not recompute or verify it.
    pub hash: Option<u32>,
    pub default_scene: Option<u32>,
    pub side_chain_follow_exists: Option<bool>,
    pub zsk_7182_applied: Option<bool>,
    /// Maximum observed label/tempo/color/parameter-value/bypass cardinality.
    pub scene_count: usize,
    pub scenes: Vec<SceneSummary>,
    pub chain_count: usize,
    /// Main Chain.models slots, including empty hash-0/absent-hash slots.
    pub model_slot_count: usize,
    /// Parameters in main Chain.models slots (controls are listed separately).
    pub parameter_count: usize,
    pub scene_value_count: usize,
    pub chains: Vec<ChainSummary>,
    pub bypass: Vec<BypassSummary>,
    pub presence: Vec<WirePresence>,
    /// Whole known message tree plus unknown group bodies, excluding opaque bytes.
    pub unknown_field_count: usize,
    pub wire_field_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SceneSummary {
    pub index: usize,
    pub label: Option<String>,
    pub tempo: Option<u32>,
    pub color: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainSummary {
    pub position: usize,
    pub row: Option<u32>,
    pub in_portid: Option<u32>,
    pub out_portid: Option<u32>,
    pub models: Vec<ModelSummary>,
    pub split_control_points: Vec<SplitControlPointsSummary>,
    pub splitter: Vec<ModelSummary>,
    pub mixer: Vec<ModelSummary>,
    pub split_bypass: Vec<SceneBypassSummary>,
    pub mix_bypass: Vec<SceneBypassSummary>,
    pub output_control: Vec<ModelSummary>,
    pub combined_splitter: Vec<ModelSummary>,
    pub input_control: Vec<ModelSummary>,
    pub presence: Vec<WirePresence>,
    pub unknown_field_count: usize,
}

/// Stored points, not a compiled routing decision. `Some(-1)` is not absence.
#[derive(Debug, Clone, Serialize)]
pub struct SplitControlPointsSummary {
    pub position: usize,
    pub split: Option<i32>,
    pub mix: Option<i32>,
    pub presence: Vec<WirePresence>,
    pub unknown_field_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BypassSummary {
    pub position: usize,
    pub row: Option<u32>,
    pub col_bypass: Vec<ColBypassSummary>,
    pub presence: Vec<WirePresence>,
    pub unknown_field_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ColBypassSummary {
    pub position: usize,
    pub column: Option<u32>,
    pub scene_mode: Option<bool>,
    pub scene_bypass: Vec<SceneBypassSummary>,
    pub presence: Vec<WirePresence>,
    pub unknown_field_count: usize,
}

/// The stored bool only; stock audio bypass polarity is not established here.
#[derive(Debug, Clone, Serialize)]
pub struct SceneBypassSummary {
    pub position: usize,
    /// `None` means no field-1 occurrence, even though proto3 defaults to false.
    pub bypass: Option<bool>,
    pub presence: Vec<WirePresence>,
    pub unknown_field_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelSummary {
    pub position: usize,
    pub hash: Option<u32>,
    pub column: Option<u32>,
    pub notify_flag: Option<bool>,
    pub sidechain_sink_flag: Option<bool>,
    pub sidechain_source_flag: Option<bool>,
    pub params: Vec<ParamSummary>,
    pub presence: Vec<WirePresence>,
    pub unknown_field_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParamSummary {
    pub position: usize,
    pub index: Option<u32>,
    pub expression: Option<i32>,
    pub expression_min: Option<f32>,
    pub expression_max: Option<f32>,
    pub scene_mode: Option<bool>,
    pub is_led: Option<bool>,
    pub values: Vec<Option<ParamValue>>,
    /// Presence per stored ParamValue, including duplicates and unknowns.
    pub value_presence: Vec<Vec<WirePresence>>,
    pub dynamic_steps: Vec<String>,
    pub dynamic_icons: Vec<String>,
    pub dynamic_metadata: Vec<String>,
    pub presence: Vec<WirePresence>,
    pub unknown_field_count: usize,
}

pub(crate) fn invalid(message: &Message, field: u32, reason: &'static str) -> Error {
    Error::InvalidKnownField {
        message: message.schema.map_or("unknown group", |s| s.name),
        field,
        reason,
    }
}

pub(crate) fn check_field(message: &Message, number: u32) -> Result<()> {
    if let Some(spec) = message.schema.and_then(|s| s.field(number)) {
        for field in message.fields.iter().filter(|f| f.number == number) {
            if field.value.wire() != spec.kind.wire()
                && !(spec.repeated && spec.kind.packable() && field.value.wire() == 2)
            {
                return Err(invalid(message, number, "wrong wire type"));
            }
        }
    }
    Ok(())
}

pub(crate) fn child<'a>(field: &'a Field, parent: &Message) -> Result<&'a Message> {
    if let Value::Message { body, .. } = &field.value {
        Ok(body)
    } else {
        Err(invalid(parent, field.number, "expected nested message"))
    }
}

fn last(message: &Message, number: u32) -> Result<Option<&Value>> {
    check_field(message, number)?;
    Ok(message
        .fields
        .iter()
        .rev()
        .find(|f| f.number == number)
        .map(|f| &f.value))
}

pub(crate) fn string(message: &Message, number: u32) -> Result<Option<String>> {
    match last(message, number)? {
        Some(Value::Bytes { data, .. }) => Ok(Some(
            std::str::from_utf8(data)
                .map_err(|_| invalid(message, number, "invalid UTF-8"))?
                .to_owned(),
        )),
        None => Ok(None),
        _ => Err(invalid(message, number, "expected string")),
    }
}

pub(crate) fn uint(message: &Message, number: u32) -> Result<Option<u32>> {
    match last(message, number)? {
        Some(Value::Varint { value, .. }) => {
            Ok(Some(u32::try_from(*value).map_err(|_| {
                invalid(message, number, "uint32 overflow")
            })?))
        }
        None => Ok(None),
        _ => Err(invalid(message, number, "expected uint32")),
    }
}

fn integer(message: &Message, number: u32) -> Result<Option<i32>> {
    match last(message, number)? {
        Some(Value::Varint { value, .. }) => Ok(Some(*value as u32 as i32)),
        None => Ok(None),
        _ => Err(invalid(message, number, "expected int32")),
    }
}

fn boolean(message: &Message, number: u32) -> Result<Option<bool>> {
    match last(message, number)? {
        Some(Value::Varint { value, .. }) => Ok(Some(*value != 0)),
        None => Ok(None),
        _ => Err(invalid(message, number, "expected bool")),
    }
}

fn float(message: &Message, number: u32) -> Result<Option<f32>> {
    match last(message, number)? {
        Some(Value::Fixed32(bytes)) => Ok(Some(f32::from_le_bytes(*bytes))),
        None => Ok(None),
        _ => Err(invalid(message, number, "expected float")),
    }
}

fn strings(message: &Message, number: u32) -> Result<Vec<String>> {
    check_field(message, number)?;
    message
        .fields
        .iter()
        .filter(|f| f.number == number)
        .map(|field| {
            if let Value::Bytes { data, .. } = &field.value {
                std::str::from_utf8(data)
                    .map(str::to_owned)
                    .map_err(|_| invalid(message, number, "invalid UTF-8"))
            } else {
                Err(invalid(message, number, "expected string"))
            }
        })
        .collect()
}

fn uints(message: &Message, number: u32) -> Result<Vec<u32>> {
    check_field(message, number)?;
    let mut values = Vec::new();
    for field in message.fields.iter().filter(|f| f.number == number) {
        match &field.value {
            Value::Varint { value, .. } => values.push(
                u32::try_from(*value).map_err(|_| invalid(message, number, "uint32 overflow"))?,
            ),
            Value::Bytes { data, .. } => {
                let mut pos = 0;
                while pos < data.len() {
                    let value = crate::wire::read_varint(data, &mut pos, 0)
                        .map_err(|_| invalid(message, number, "invalid packed varints"))?;
                    values.push(
                        u32::try_from(value)
                            .map_err(|_| invalid(message, number, "uint32 overflow"))?,
                    );
                }
            }
            _ => return Err(invalid(message, number, "expected uint32 or packed uint32")),
        }
    }
    Ok(values)
}

fn children(message: &Message, number: u32) -> Result<Vec<&Message>> {
    check_field(message, number)?;
    message
        .fields
        .iter()
        .filter(|f| f.number == number)
        .map(|f| child(f, message))
        .collect()
}

pub(crate) fn presence(message: &Message) -> Vec<WirePresence> {
    let mut presence: Vec<WirePresence> = Vec::new();
    let mut indices: BTreeMap<u32, usize> = BTreeMap::new();
    for field in &message.fields {
        if let Some(&index) = indices.get(&field.number) {
            let item = &mut presence[index];
            item.occurrences += 1;
            if !item.wire_types.contains(&field.value.wire()) {
                item.wire_types.push(field.value.wire());
            }
        } else {
            let spec = message.schema.and_then(|s| s.field(field.number));
            indices.insert(field.number, presence.len());
            presence.push(WirePresence {
                number: field.number,
                name: spec.map(|s| s.name),
                optional: spec.is_some_and(|s| s.optional),
                occurrences: 1,
                wire_types: vec![field.value.wire()],
            });
        }
    }
    presence
}

fn counts(message: &Message) -> (usize, usize) {
    let mut total = message.fields.len();
    let mut unknown = message
        .fields
        .iter()
        .filter(|f| message.schema.and_then(|s| s.field(f.number)).is_none())
        .count();
    for field in &message.fields {
        if let Value::Message { body, .. } | Value::Group { body, .. } = &field.value {
            let (child_total, child_unknown) = counts(body);
            total += child_total;
            unknown += child_unknown;
        }
    }
    (total, unknown)
}

pub(crate) fn parameter_value(message: &Message) -> Result<Option<ParamValue>> {
    for number in 1..=3 {
        check_field(message, number)?;
    }
    match message
        .fields
        .iter()
        .rev()
        .find(|f| (1..=3).contains(&f.number))
    {
        Some(field) => match &field.value {
            Value::Varint { value, .. } => Ok(Some(ParamValue::Int(*value as u32 as i32))),
            Value::Fixed32(bytes) => Ok(Some(ParamValue::Float(f32::from_le_bytes(*bytes)))),
            Value::Bytes { data, .. } => Ok(Some(ParamValue::String(
                std::str::from_utf8(data)
                    .map_err(|_| invalid(message, 3, "invalid UTF-8"))?
                    .to_owned(),
            ))),
            _ => Err(invalid(message, field.number, "invalid oneof value")),
        },
        None => Ok(None),
    }
}

fn parameter(message: &Message, position: usize) -> Result<ParamSummary> {
    let values = children(message, 5)?;
    Ok(ParamSummary {
        position,
        index: uint(message, 6)?,
        expression: integer(message, 1)?,
        expression_min: float(message, 2)?,
        expression_max: float(message, 3)?,
        scene_mode: boolean(message, 4)?,
        is_led: boolean(message, 7)?,
        values: values
            .iter()
            .map(|m| parameter_value(m))
            .collect::<Result<_>>()?,
        value_presence: values.iter().map(|m| presence(m)).collect(),
        dynamic_steps: strings(message, 8)?,
        dynamic_icons: strings(message, 9)?,
        dynamic_metadata: strings(message, 10)?,
        presence: presence(message),
        unknown_field_count: counts(message).1,
    })
}

fn model(message: &Message, position: usize) -> Result<ModelSummary> {
    Ok(ModelSummary {
        position,
        hash: uint(message, 1)?,
        column: uint(message, 5)?,
        notify_flag: boolean(message, 6)?,
        sidechain_sink_flag: boolean(message, 7)?,
        sidechain_source_flag: boolean(message, 8)?,
        params: children(message, 2)?
            .into_iter()
            .enumerate()
            .map(|(i, m)| parameter(m, i))
            .collect::<Result<_>>()?,
        presence: presence(message),
        unknown_field_count: counts(message).1,
    })
}

fn models(message: &Message, number: u32) -> Result<Vec<ModelSummary>> {
    children(message, number)?
        .into_iter()
        .enumerate()
        .map(|(i, m)| model(m, i))
        .collect()
}

fn split_control_points(message: &Message, position: usize) -> Result<SplitControlPointsSummary> {
    Ok(SplitControlPointsSummary {
        position,
        split: integer(message, 1)?,
        mix: integer(message, 2)?,
        presence: presence(message),
        unknown_field_count: counts(message).1,
    })
}

fn scene_bypasses(message: &Message, number: u32) -> Result<Vec<SceneBypassSummary>> {
    children(message, number)?
        .into_iter()
        .enumerate()
        .map(|(position, message)| {
            Ok(SceneBypassSummary {
                position,
                bypass: boolean(message, 1)?,
                presence: presence(message),
                unknown_field_count: counts(message).1,
            })
        })
        .collect()
}

fn col_bypass(message: &Message, position: usize) -> Result<ColBypassSummary> {
    Ok(ColBypassSummary {
        position,
        column: uint(message, 2)?,
        scene_mode: boolean(message, 3)?,
        scene_bypass: scene_bypasses(message, 1)?,
        presence: presence(message),
        unknown_field_count: counts(message).1,
    })
}

fn bypass(message: &Message, position: usize) -> Result<BypassSummary> {
    Ok(BypassSummary {
        position,
        row: uint(message, 2)?,
        col_bypass: children(message, 1)?
            .into_iter()
            .enumerate()
            .map(|(i, m)| col_bypass(m, i))
            .collect::<Result<_>>()?,
        presence: presence(message),
        unknown_field_count: counts(message).1,
    })
}

fn chain(message: &Message, position: usize) -> Result<ChainSummary> {
    Ok(ChainSummary {
        position,
        row: uint(message, 14)?,
        in_portid: uint(message, 1)?,
        out_portid: uint(message, 2)?,
        models: models(message, 5)?,
        split_control_points: children(message, 6)?
            .into_iter()
            .enumerate()
            .map(|(i, m)| split_control_points(m, i))
            .collect::<Result<_>>()?,
        splitter: models(message, 7)?,
        mixer: models(message, 8)?,
        split_bypass: scene_bypasses(message, 10)?,
        mix_bypass: scene_bypasses(message, 11)?,
        output_control: models(message, 9)?,
        combined_splitter: models(message, 12)?,
        input_control: models(message, 13)?,
        presence: presence(message),
        unknown_field_count: counts(message).1,
    })
}

pub(crate) fn summarize(root: &Message) -> Result<PresetSummary> {
    let chains: Vec<_> = children(root, 11)?
        .into_iter()
        .enumerate()
        .map(|(i, m)| chain(m, i))
        .collect::<Result<_>>()?;
    let bypass: Vec<_> = children(root, 18)?
        .into_iter()
        .enumerate()
        .map(|(i, m)| bypass(m, i))
        .collect::<Result<_>>()?;
    let labels = strings(root, 15)?;
    let tempos = uints(root, 14)?;
    let colors = uints(root, 31)?;
    let mut scene_count = labels.len().max(tempos.len()).max(colors.len());
    let mut model_slot_count = 0;
    let mut parameter_count = 0;
    let mut scene_value_count = 0;
    for row in &bypass {
        for column in &row.col_bypass {
            scene_count = scene_count.max(column.scene_bypass.len());
        }
    }
    for chain in &chains {
        scene_count = scene_count
            .max(chain.split_bypass.len())
            .max(chain.mix_bypass.len());
        model_slot_count += chain.models.len();
        for model in &chain.models {
            parameter_count += model.params.len();
            for param in &model.params {
                scene_value_count += param.values.len();
            }
        }
        for group in [
            &chain.models,
            &chain.splitter,
            &chain.mixer,
            &chain.output_control,
            &chain.combined_splitter,
            &chain.input_control,
        ] {
            for model in group {
                for param in &model.params {
                    scene_count = scene_count.max(param.values.len());
                }
            }
        }
    }
    let scenes = (0..scene_count)
        .map(|index| SceneSummary {
            index,
            label: labels.get(index).cloned(),
            tempo: tempos.get(index).copied(),
            color: colors.get(index).copied(),
        })
        .collect();
    let (wire_field_count, unknown_field_count) = counts(root);
    Ok(PresetSummary {
        name: string(root, 2)?,
        hash: uint(root, 3)?,
        default_scene: uint(root, 7)?,
        side_chain_follow_exists: boolean(root, 36)?,
        zsk_7182_applied: boolean(root, 37)?,
        scene_count,
        scenes,
        chain_count: chains.len(),
        model_slot_count,
        parameter_count,
        scene_value_count,
        chains,
        bypass,
        presence: presence(root),
        unknown_field_count,
        wire_field_count,
    })
}

pub(crate) fn matches_wire(kind: Kind, repeated: bool, wire: u8) -> bool {
    kind.wire() == wire || (repeated && kind.packable() && wire == 2)
}
