//! The stock block serializer `0x67cb3c` in the form both resync builders call it
//! (`0x67cb3c(model, block, 1, 0, 0)`), and the DefaultParameters (19) and ModelPreset (71)
//! resync entries built with it.
//!
//! The input is the stored form of a runtime block (`tetrad_preset::Model`): `params` are the
//! runtime parameters without BYPASS (the last runtime parameter), `bypass_expression[0]` is
//! the BYPASS expression and `expression_bypass_info` holds the block switches in the stock
//! loader's positions (`0x681360`).

use tetrad_preset::{INPUT_CONTROL_ID, OUTPUT_CONTROL_ID};

use crate::codecs::{
    DefaultParametersModel, Expression, ExpressionBypassInfo, Model, ModelPreset, ModelPresetID,
    Param, ParamValue,
};

/// The `ParamValue` case `0x67cb3c` writes for one parameter. Stock picks it from the
/// runtime parameter type (`param +0x8`): 2 (`int`) int, 9 (`string`) string, any other
/// type float.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ValueCase {
    #[default]
    Float,
    Int,
    String,
}

/// A stored-form runtime block and the value case of each stored parameter. A missing
/// case is [`ValueCase::Float`] (MUTE and every non-`int`, non-`string` parameter).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TypedBlock {
    pub model: tetrad_preset::Model,
    pub cases: Vec<ValueCase>,
}

/// A DefaultParameters (19) source: the library capture found for one `default_param` key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CaptureDefault {
    /// The capture's id (`+4`); stock writes it as the entry id (`toUtf8`).
    pub id: String,
    /// The block `0x704530` builds for the capture.
    pub block: TypedBlock,
}

/// One ModelPreset (71) record of `0x1f6280` (20 bytes: id, factory flag, hash, name,
/// default flag).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelPresetRecord {
    pub id: String,
    pub is_factory: bool,
    pub hash: u32,
    pub name: String,
    /// `Some` marks a default entry: the block `0x1f7e74` builds from the record. Stock
    /// serializes a block for default entries only.
    pub default_block: Option<TypedBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlockError {
    #[error("model {model} parameter {index} has no scene 0 value")]
    MissingValue { model: u32, index: usize },
    #[error("model {model} parameter {index}: a stored {from} value as {to:?} is not recovered")]
    Conversion {
        model: u32,
        index: usize,
        from: &'static str,
        to: ValueCase,
    },
    #[error("model {model} needs stored bypass switch entry {entry}")]
    MissingSwitch { model: u32, entry: usize },
    /// Model 23000 stores its switches at `+0xc8`/`+0xd4`; the `+0xbc` switch that a
    /// bypass expression adds has no stored form.
    #[error("model 23000 has a bypass expression but no stored +0xbc switch")]
    UnstoredSwitch,
}

/// `0x67cb3c(model, block, 1, 0, 0)`: the hash, every stored parameter with its four
/// controls and its scene 0 value, the BYPASS expression, then the switches.
pub fn serialize(block: &tetrad_preset::Model, cases: &[ValueCase]) -> Result<Model, BlockError> {
    let model = block.hash.unwrap_or(0);
    let params = block
        .params
        .iter()
        .enumerate()
        .map(|(index, param)| {
            let case = cases.get(index).copied().unwrap_or_default();
            let value = param
                .value(0)
                .ok_or(BlockError::MissingValue { model, index })?;
            Ok(Param {
                // Absent stored controls are the runtime defaults (`Param::new`).
                expression: Some(param.expression.unwrap_or(0)),
                expression_min: Some(param.expression_min.unwrap_or(0.0)),
                expression_max: Some(param.expression_max.unwrap_or(1.0)),
                scene_mode: Some(param.scene_mode.unwrap_or(false)),
                param_values: vec![convert(value, case).ok_or(BlockError::Conversion {
                    model,
                    index,
                    from: value_name(value),
                    to: case,
                })?],
                ..Param::default()
            })
        })
        .collect::<Result<_, BlockError>>()?;
    let (code, min, max) = block
        .bypass_expression
        .first()
        .map_or((0, 0.0, 1.0), |bypass| {
            (
                bypass.expression,
                bypass.expression_min,
                bypass.expression_max,
            )
        });
    let entry = |entry: usize| {
        block
            .expression_bypass_info
            .get(entry)
            .map(switch)
            .ok_or(BlockError::MissingSwitch { model, entry })
    };
    let mut switches = Vec::new();
    if code != 0 {
        if model == OUTPUT_CONTROL_ID {
            return Err(BlockError::UnstoredSwitch);
        }
        switches.push(entry(0)?);
    }
    match model {
        // `+0xc8` is entry 0; the loader writes every later entry to `+0xd4`.
        OUTPUT_CONTROL_ID => {
            switches.push(entry(0)?);
            switches.push(entry(block.expression_bypass_info.len().max(2) - 1)?);
        }
        INPUT_CONTROL_ID => switches.push(entry(0)?),
        _ => {}
    }
    Ok(Model {
        hash: Some(model),
        params,
        bypass_expression: vec![Expression {
            expression: implicit(code),
            expression_min: implicit(min),
            expression_max: implicit(max),
        }],
        expression_bypass_info: switches,
        ..Model::default()
    })
}

/// `0x15d890` key pass over `SELECT DISTINCT model_id FROM default_param`: std::set order
/// (QString `<`, UTF-16 units), then `toInt(&ok, 10)`. A number other than 14000 or 14001
/// is skipped; the capture models and every non-number go to the library lookup.
pub fn default_parameter_keys(mut keys: Vec<String>) -> Vec<String> {
    keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    keys.dedup();
    keys.retain(|key| matches!(key.trim().parse::<i32>(), Err(_) | Ok(14000 | 14001)));
    keys
}

/// `0x15d890` entry: the capture id and its serialized block. proto3 `id` is not written
/// when empty.
pub fn default_parameters_entry(
    capture: &CaptureDefault,
) -> Result<DefaultParametersModel, BlockError> {
    Ok(DefaultParametersModel {
        id: implicit(capture.id.clone()),
        model: Some(serialize(&capture.block.model, &capture.block.cases)?),
    })
}

/// `0x1cf8dc(entry, record, 1)`, the resync form: id (`0x1cf6c0`), name and default flag
/// always; the serialized block for a default entry. With flag 1 the special id
/// (`SpecialFactoryModelPresetID`) also gets its block.
pub fn model_preset_entry(record: &ModelPresetRecord) -> Result<ModelPreset, BlockError> {
    let model = record
        .default_block
        .as_ref()
        .map(|block| serialize(&block.model, &block.cases))
        .transpose()?;
    Ok(ModelPreset {
        id: Some(ModelPresetID {
            value: Some(to_ascii(&record.id)),
            is_factory: Some(record.is_factory),
            hash: Some(record.hash),
        }),
        name: Some(to_ascii(&record.name)),
        is_default: Some(record.default_block.is_some()),
        model,
    })
}

/// Qt 4.8 `QString::toAscii` without a C-string codec: Latin-1, other characters `?`.
/// Owned: U+0080..=U+00FF stay UTF-8 here; stock writes single Latin-1 bytes, which a
/// proto3 string parser rejects.
fn to_ascii(text: &str) -> String {
    text.chars()
        .map(|c| if u32::from(c) > 0xff { '?' } else { c })
        .collect()
}

/// proto3 implicit presence: zero is not written. `-0.0 == 0.0` and `NaN != 0.0` match the
/// stock float test `!(v <= 0 && v >= 0)` (`0x8aa9d0`).
fn implicit<T: Default + PartialEq>(value: T) -> Option<T> {
    (value != T::default()).then_some(value)
}

/// Runtime switch `{type, invert, latch, delay}` (12 bytes) to the implicit-field message.
fn switch(stored: &tetrad_preset::ExpressionBypassInfo) -> ExpressionBypassInfo {
    ExpressionBypassInfo {
        bypass_type: implicit(stored.kind),
        invert: implicit(stored.invert),
        delay_ms: implicit(stored.delay_ms),
        latch_emulation: implicit(stored.latch_emulation),
    }
}

/// `QVariant::toInt` / `toFloat` over the runtime value. Non-string runtime values are
/// `QMetaType::Float` (`0x1f7b10`, loader copies), so the int case is `qRound64(float)`
/// truncated to `int`. Conversions from or to text are not recovered.
fn convert(value: &tetrad_preset::ParamValue, case: ValueCase) -> Option<ParamValue> {
    use tetrad_preset::ParamValue as Stored;
    Some(match (case, value) {
        (ValueCase::Float, Stored::Float(v)) => ParamValue::float(*v),
        (ValueCase::Float, Stored::Int(v)) => ParamValue::float(*v as f32),
        (ValueCase::Int, Stored::Int(v)) => ParamValue::int(*v),
        (ValueCase::Int, Stored::Float(v)) => ParamValue::int(q_round64(*v) as i32),
        (ValueCase::String, Stored::String(text)) => ParamValue::string(text.clone()),
        _ => return None,
    })
}

fn value_name(value: &tetrad_preset::ParamValue) -> &'static str {
    match value {
        tetrad_preset::ParamValue::Int(_) => "int",
        tetrad_preset::ParamValue::Float(_) => "float",
        tetrad_preset::ParamValue::String(_) => "string",
    }
}

/// Qt 4.8 `qRound64(qreal)`; `qreal` is `float` on ARM (inferred for this build). Owned:
/// values past the `i64` range (undefined in Qt) saturate.
fn q_round64(d: f32) -> i64 {
    if d >= 0.0 {
        (d + 0.5) as i64
    } else {
        let below = (d - 1.0) as i64;
        ((d - below as f32 + 0.5) as i64).saturating_add(below)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::CodecLimits;
    use tetrad_preset::{Expression as StoredExpression, ExpressionBypassInfo as StoredSwitch};
    use tetrad_preset::{Param as StoredParam, ParamValue as Stored, SceneValue};

    fn stored_switch(kind: u32) -> StoredSwitch {
        StoredSwitch {
            kind,
            invert: true,
            ..StoredSwitch::default()
        }
    }

    /// The `0x67cb3c(.., 1, 0, 0)` field table on one synthetic block: hash always; per
    /// parameter the four controls always, then only the scene 0 value in its runtime case
    /// (int = `qRound64`), never `index`, `is_led` or the dynamic lists; one BYPASS
    /// expression with zero (also -0.0) fields omitted; the `+0xbc` switch because the
    /// bypass code is set; no column or flags.
    #[test]
    fn serializer_writes_the_stock_field_table() {
        let mut first = StoredParam::new(Stored::Float(0.25));
        first.set_expression(1, 0.1, 0.9).unwrap();
        first.scene_mode = Some(true);
        first.values[1].value = Some(Stored::Float(0.75));
        first.index = Some(3);
        first.is_led = Some(true);
        first.dynamic_steps = vec!["step".into()];
        let mute = StoredParam {
            values: vec![SceneValue {
                value: Some(Stored::Float(0.0)),
                ..SceneValue::default()
            }],
            ..StoredParam::default()
        };
        let block = tetrad_preset::Model {
            hash: Some(7),
            params: vec![
                first,
                StoredParam::new(Stored::Float(0.5)),
                StoredParam::new(Stored::String("ir".into())),
                mute,
            ],
            bypass_expression: vec![StoredExpression {
                expression: 2,
                expression_min: -0.0,
                expression_max: 1.0,
                ..StoredExpression::default()
            }],
            expression_bypass_info: vec![stored_switch(1)],
            column: Some(3),
            notify_flag: Some(true),
            ..tetrad_preset::Model::default()
        };
        let cases = [ValueCase::Float, ValueCase::Int, ValueCase::String];
        let model = serialize(&block, &cases).unwrap();
        let bytes = model.encode(CodecLimits::default()).unwrap();
        let controls = |code: u8, min: [u8; 4], max: [u8; 4], scene_mode: u8| {
            [
                &[0x08, code, 0x15][..],
                &min,
                &[0x1d],
                &max,
                &[0x20, scene_mode],
            ]
            .concat()
        };
        let defaults = controls(0, [0; 4], 1.0f32.to_le_bytes(), 0);
        let param = |controls: &[u8], value: &[u8]| {
            let body = [controls, &[0x2a, value.len() as u8], value].concat();
            [&[0x12, body.len() as u8][..], &body].concat()
        };
        let expected = [
            vec![0x08, 7],
            param(
                &controls(1, 0.1f32.to_le_bytes(), 0.9f32.to_le_bytes(), 1),
                &[0x15, 0, 0, 0x80, 0x3e],
            ),
            param(&defaults, &[0x08, 1]),
            param(&defaults, &[0x1a, 2, b'i', b'r']),
            param(&defaults, &[0x15, 0, 0, 0, 0]),
            vec![0x1a, 7, 0x08, 2, 0x1d, 0, 0, 0x80, 0x3f],
            vec![0x22, 4, 0x08, 1, 0x10, 1],
        ]
        .concat();
        assert_eq!(bytes, expected);
        let text = StoredParam::new(Stored::String("x".into()));
        let refused = serialize(
            &tetrad_preset::Model {
                params: vec![text],
                ..block
            },
            &[ValueCase::Int],
        );
        assert_eq!(
            refused,
            Err(BlockError::Conversion {
                model: 7,
                index: 0,
                from: "string",
                to: ValueCase::Int
            })
        );
    }

    /// Switch tail by model (`0x67cb3c` after the BYPASS expression, positions from the
    /// loader `0x681360`): 23000 sends entry 0 (`+0xc8`) and its last entry (`+0xd4`);
    /// 28000 sends entry 0 (`+0xbc`) after the bypass-code switch, so twice with a code.
    #[test]
    fn lane_control_switches_follow_the_loader_positions() {
        let lane = |model: u32, code: i32, kinds: &[u32]| tetrad_preset::Model {
            hash: Some(model),
            bypass_expression: vec![StoredExpression {
                expression: code,
                ..StoredExpression::default()
            }],
            expression_bypass_info: kinds.iter().copied().map(stored_switch).collect(),
            ..tetrad_preset::Model::default()
        };
        let kinds = |model: Result<Model, BlockError>| {
            let switches = model.unwrap().expression_bypass_info;
            switches.iter().map(|s| s.bypass_type).collect::<Vec<_>>()
        };
        let output = serialize(&lane(OUTPUT_CONTROL_ID, 0, &[1, 2, 3]), &[]);
        assert_eq!(kinds(output), [Some(1), Some(3)]);
        let input = serialize(&lane(INPUT_CONTROL_ID, 4, &[2]), &[]);
        assert_eq!(kinds(input), [Some(2), Some(2)]);
        assert_eq!(kinds(serialize(&lane(9, 0, &[2]), &[])), []);
        assert_eq!(
            serialize(&lane(OUTPUT_CONTROL_ID, 0, &[1]), &[]),
            Err(BlockError::MissingSwitch {
                model: OUTPUT_CONTROL_ID,
                entry: 1
            })
        );
        assert_eq!(
            serialize(&lane(OUTPUT_CONTROL_ID, 4, &[1, 2]), &[]),
            Err(BlockError::UnstoredSwitch)
        );
        assert_eq!(
            serialize(&lane(9, 4, &[]), &[]),
            Err(BlockError::MissingSwitch { model: 9, entry: 0 })
        );
    }

    /// `qRound64` rounds half up, and a default row text such as "-inf" on an `int`
    /// parameter saturates instead of overflowing (owned).
    #[test]
    fn q_round64_rounds_half_up_and_saturates() {
        assert_eq!((q_round64(-2.5), q_round64(2.5)), (-2, 3));
        assert_eq!(q_round64(f32::NEG_INFINITY), i64::MIN);
        assert_eq!(q_round64(-1e19), i64::MIN);
    }
}
