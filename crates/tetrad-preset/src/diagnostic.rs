use std::collections::BTreeMap;

use serde::Serialize;

use crate::{
    schema::{Kind, MessageSchema},
    summary,
    wire::{Message, Value, read_varint},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    /// Schema field names and zero-based occurrence indices.
    pub path: String,
    pub message: String,
}

fn emit(
    out: &mut Vec<Diagnostic>,
    severity: Severity,
    code: &'static str,
    path: &str,
    message: impl Into<String>,
) {
    out.push(Diagnostic {
        severity,
        code,
        path: path.to_owned(),
        message: message.into(),
    });
}

/// ARM default NaN: the result of an invalid float operation such as 0/0.
const DEFAULT_NAN: u32 = 0x7fc0_0000;

pub(crate) fn validate(root: &Message) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    visit(root, "BinaryPreset", &mut out);
    out
}

fn scalar(kind: Kind, value: &Value, path: &str, out: &mut Vec<Diagnostic>) {
    match (kind, value) {
        (Kind::String, Value::Bytes { data, .. }) if std::str::from_utf8(data).is_err() => {
            emit(
                out,
                Severity::Error,
                "invalid_utf8",
                path,
                "Known string contains invalid UTF-8.",
            );
        }
        (Kind::Float, Value::Fixed32(bytes)) if !f32::from_le_bytes(*bytes).is_finite() => {
            emit(
                out,
                Severity::Error,
                "non_finite_float",
                path,
                "Known float contains NaN or infinity.",
            );
        }
        (Kind::Uint32, Value::Varint { value, .. }) if *value > u64::from(u32::MAX) => {
            emit(
                out,
                Severity::Error,
                "uint32_overflow",
                path,
                "Value does not fit uint32.",
            );
        }
        (Kind::Int32, Value::Varint { value, .. }) => {
            let signed = *value as u32 as i32;
            if *value > u64::from(u32::MAX) && *value != i64::from(signed) as u64 {
                emit(
                    out,
                    Severity::Warning,
                    "int32_truncation",
                    path,
                    "Decoder truncates this value to the low 32 bits.",
                );
            }
        }
        (Kind::Bool, Value::Varint { value, .. }) if *value > 1 => {
            emit(
                out,
                Severity::Warning,
                "noncanonical_bool",
                path,
                "Nonzero bool decodes as true; raw value is preserved.",
            );
        }
        (Kind::Enum("StompModeAssignment.Type"), Value::Varint { value, .. }) if *value > 1 => {
            emit(
                out,
                Severity::Info,
                "unknown_enum",
                path,
                "Unknown enum value is preserved.",
            );
        }
        _ => {}
    }
}

fn packed(kind: Kind, data: &[u8], path: &str, out: &mut Vec<Diagnostic>) {
    if kind == Kind::Float {
        if !data.len().is_multiple_of(4) {
            emit(
                out,
                Severity::Error,
                "invalid_packed_size",
                path,
                "Packed float byte length must be a multiple of four.",
            );
            return;
        }
        for bytes in data.chunks_exact(4) {
            scalar(
                kind,
                &Value::Fixed32(bytes.try_into().expect("four bytes")),
                path,
                out,
            );
        }
    } else {
        let mut pos = 0;
        while pos < data.len() {
            let start = pos;
            let value = match read_varint(data, &mut pos, 0) {
                Ok(value) => value,
                Err(_) => {
                    emit(
                        out,
                        Severity::Error,
                        "invalid_packed_varint",
                        path,
                        format!("Invalid packed varint at payload byte {start}."),
                    );
                    return;
                }
            };
            scalar(
                kind,
                &Value::Varint {
                    raw: Default::default(),
                    value,
                },
                path,
                out,
            );
        }
    }
}

fn visit(message: &Message, path: &str, out: &mut Vec<Diagnostic>) {
    let mut occurrences: BTreeMap<u32, usize> = BTreeMap::new();
    let mut unknown = 0;
    let mut oneof = 0;
    for field in &message.fields {
        let occurrence = occurrences.entry(field.number).or_default();
        let spec = message.schema.and_then(|s| s.field(field.number));
        let field_path = match spec {
            Some(spec) => format!("{path}.{}[{occurrence}]", spec.name),
            None => format!("{path}.unknown_{}[{occurrence}]", field.number),
        };
        *occurrence += 1;
        if let Some(spec) = spec {
            if !summary::matches_wire(spec.kind, spec.repeated, field.value.wire()) {
                emit(
                    out,
                    Severity::Error,
                    "known_wire_type",
                    &field_path,
                    format!(
                        "Expected wire type {}, found {}.",
                        spec.kind.wire(),
                        field.value.wire()
                    ),
                );
            } else if spec.repeated && spec.kind.packable() && field.value.wire() == 2 {
                if let Value::Bytes { data, .. } = &field.value {
                    packed(spec.kind, data, &field_path, out);
                }
            } else if message.schema.is_some_and(|s| s.name == "ParamValue")
                && matches!(field.value, Value::Fixed32(bytes) if u32::from_le_bytes(bytes) == DEFAULT_NAN)
            {
                // Stock-accepted: the loader copies parameter float bits without a
                // finite check (VA 0x680a24..0x680e30), and factory presets ship
                // this exact pattern. Other non-finite patterns stay errors.
                emit(
                    out,
                    Severity::Warning,
                    "stock_default_nan",
                    &field_path,
                    "Parameter holds the ARM default NaN that stock presets store and load unchanged.",
                );
            } else {
                scalar(spec.kind, &field.value, &field_path, out);
            }
            if message.schema.is_some_and(|s| s.name == "ParamValue") {
                oneof += 1;
            }
        } else {
            unknown += 1;
        }
        if let Value::Message { body, .. } | Value::Group { body, .. } = &field.value {
            visit(body, &field_path, out);
        }
    }
    if let Some(schema) = message.schema {
        for (number, count) in &occurrences {
            if let Some(spec) = schema.field(*number)
                && !spec.repeated
                && *count > 1
            {
                emit(
                    out,
                    Severity::Info,
                    "duplicate_scalar",
                    path,
                    format!(
                        "{} has {count} occurrences; the final occurrence supplies its value.",
                        spec.name
                    ),
                );
            }
        }
        sizes(message, schema, path, out);
    }
    if oneof > 1 {
        emit(
            out,
            Severity::Info,
            "oneof_overrides",
            path,
            format!("{oneof} known oneof values; the final occurrence is active."),
        );
    }
    if unknown > 0 {
        emit(
            out,
            Severity::Info,
            "unknown_fields",
            path,
            format!("{unknown} direct unknown field occurrences are preserved."),
        );
    }
}

fn sizes(message: &Message, schema: &MessageSchema, path: &str, out: &mut Vec<Diagnostic>) {
    match schema.name {
        "BinaryPreset" => {
            let version_fields = [22, 23, 24].map(|number| message.positions(number).count());
            let present = version_fields.iter().filter(|&&count| count > 0).count();
            if present != 0 && present != 3 {
                emit(
                    out,
                    Severity::Warning,
                    "partial_version",
                    path,
                    "Only some of created/modified/oldest-compatible version metadata is present.",
                );
            }
            if summary::uint(message, 7)
                .ok()
                .flatten()
                .is_some_and(|index| index > 7)
            {
                emit(
                    out,
                    Severity::Error,
                    "default_scene_range",
                    path,
                    "Default scene exceeds the recovered 0..7 range.",
                );
            }
            let labels = message.positions(15).count();
            if labels > 8 {
                emit(
                    out,
                    Severity::Warning,
                    "scene_cardinality",
                    path,
                    "More than eight scene labels; compatibility with the recovered runtime is unverified.",
                );
            } else if labels < 8 {
                emit(
                    out,
                    Severity::Warning,
                    "stock_short_scene_labels",
                    path,
                    "Fewer than eight scene labels; the stock label getter does not check its index.",
                );
            }
            if packed_count(message, 31) > 8 {
                emit(
                    out,
                    Severity::Error,
                    "stock_scene_overflow",
                    path,
                    "More than eight scene colors; the stock loader writes past its color table (VA 0x683d30).",
                );
            }
            if version_fields[0] > 0 && (version_fields[1] == 0 || version_fields[2] == 0) {
                emit(
                    out,
                    Severity::Error,
                    "stock_missing_version",
                    path,
                    "Created version without modified/oldest version; the stock loader reads an empty list (VA 0x681e3c).",
                );
            }
            if message.positions(26).count() > 0 && message.positions(25).count() == 0 {
                emit(
                    out,
                    Severity::Error,
                    "stock_description_without_cloud_id",
                    path,
                    "Description without cloud ID; the stock loader reads an empty cloud ID list (VA 0x6840a8).",
                );
            }
        }
        "Param" => {
            let values = message.positions(5).count();
            if values > 8 {
                emit(
                    out,
                    Severity::Error,
                    "stock_scene_overflow",
                    path,
                    "More than eight parameter scene values; the stock loader writes past its scene vectors.",
                );
            } else if values < 8 {
                emit(
                    out,
                    Severity::Warning,
                    "stock_short_scene_values",
                    path,
                    "Fewer than eight parameter scene values; stock leaves the other scenes invalid.",
                );
            }
        }
        "ParamValue" if !message.fields.iter().any(|f| (1..=3).contains(&f.number)) => {
            emit(
                out,
                Severity::Warning,
                "stock_empty_value",
                path,
                "Parameter value without a type; the stock loader reads it as float 0.0.",
            );
        }
        "Chain" | "Bypass" => range(
            message,
            if schema.name == "Chain" { 14 } else { 2 },
            3,
            "row",
            path,
            out,
        ),
        "Model" | "ColBypass" => range(
            message,
            if schema.name == "Model" { 5 } else { 2 },
            7,
            "column",
            path,
            out,
        ),
        "LegacyStompModeStompData" | "StompModeAssignment" => {
            range(message, 1, 3, "row", path, out);
            range(message, 2, 7, "column", path, out);
            if schema.name == "StompModeAssignment" {
                range(message, 3, 10, "stomp index", path, out);
            }
        }
        _ => {}
    }
}

/// Elements of a repeated scalar, packed or not; malformed payloads count as read.
fn packed_count(message: &Message, number: u32) -> usize {
    message
        .fields
        .iter()
        .filter(|f| f.number == number)
        .map(|field| match &field.value {
            Value::Bytes { data, .. } => {
                let mut pos = 0;
                let mut count = 0;
                while pos < data.len() && read_varint(data, &mut pos, 0).is_ok() {
                    count += 1;
                }
                count
            }
            _ => 1,
        })
        .sum()
}

fn range(
    message: &Message,
    number: u32,
    max: u32,
    label: &str,
    path: &str,
    out: &mut Vec<Diagnostic>,
) {
    if summary::uint(message, number)
        .ok()
        .flatten()
        .is_some_and(|value| value > max)
    {
        emit(
            out,
            Severity::Warning,
            "coordinate_range",
            path,
            format!("{label} exceeds recovered 0..{max} range; graph validity is not assessed."),
        );
    }
}
