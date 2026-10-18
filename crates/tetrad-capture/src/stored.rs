//! Stored capture records as the stock library loads them (VA 0x7ed2c8): the metadata
//! `version` picks the format, and V1 rawdata becomes the DSP runtime data.
//!
//! Decryption stays with the caller; this module never holds a key. Stock decrypts the metadata and
//! V1 rawdata with the device cipher (ZC mode 1, seed ‖ serial, `0x6abcac`) and NC2 rawdata with
//! the DSP boot vector `0x27` (ZC mode 2, controller `+0xf04c`, `0x515a48` → `0x6acc9c`).

use crate::proto::{Fields, Malformed, Value};
use crate::v1::{Limits, RuntimeData};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoredError {
    #[error("capture metadata is not a JSON object")]
    Metadata,
    #[error(transparent)]
    Malformed(#[from] Malformed),
    #[error("V1 capture {field}: {found} values, expected {expected}")]
    Count {
        field: &'static str,
        found: usize,
        expected: usize,
    },
    #[error(transparent)]
    Runtime(#[from] crate::Error),
}

/// Record format. Stock compares the metadata `version` with `"2"` (VA 0x11e7de4 in
/// 0x7ed2c8); any other text, and a missing key (default `"1"`, 0x7efc38), is V1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordVersion {
    V1,
    Nc2,
}

/// The decrypted capture metadata fields the device needs (type-2 branch of 0x7efc38).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureMetadata {
    pub name: String,
    pub version: RecordVersion,
}

impl CaptureMetadata {
    pub fn from_json(bytes: &[u8]) -> Result<Self, StoredError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| StoredError::Metadata)?;
        let object = value.as_object().ok_or(StoredError::Metadata)?;
        let text = |key| object.get(key).and_then(serde_json::Value::as_str);
        Ok(Self {
            name: text("name").unwrap_or_default().to_owned(),
            version: match text("version") {
                Some("2") => RecordVersion::Nc2,
                _ => RecordVersion::V1,
            },
        })
    }
}

/// Metadata `deviceType` text of a capture record device code (VA 0x7ecc18 in 0x7ee62c).
pub fn device_type_name(device: i32) -> &'static str {
    match device {
        0 => "default",
        1 => "amp_head",
        2 => "amp_combo",
        3 => "amp_and_cab",
        4 => "cab",
        5 => "pedal",
        6 => "fuzz",
        7 => "compressor",
        8 => "overdrive",
        _ => "unknown",
    }
}

/// Metadata `instrumentType` text of an instrument code (VA 0x7ec4c4 in 0x7ee62c).
pub fn instrument_type_name(instrument: i32) -> &'static str {
    match instrument {
        0 => "instrument_not_set",
        1 => "guitar",
        2 => "bass",
        3 => "synth",
        4 => "vocal",
        5 => "other",
        _ => "unknown",
    }
}

/// Device code of a metadata `deviceType` text (VA 0x7ec994 in the cloud product parser
/// 0x722464); the inverse of [`device_type_name`].
pub fn device_type_code(name: &str) -> Option<i32> {
    (0..=8).find(|&code| device_type_name(code) == name)
}

/// Instrument code of a metadata `instrumentType` text (VA 0x7ec714 in 0x722464); the
/// inverse of [`instrument_type_name`].
pub fn instrument_type_code(name: &str) -> Option<i32> {
    (0..=5).find(|&code| instrument_type_name(code) == name)
}

/// One `layerWeightsAndBiases` message.
#[derive(Debug, Default)]
struct LayerTensors {
    weights: Vec<f32>,
    biases: Vec<f32>,
}

/// Converter VA 0x6ec290: decrypted `captureProto.Capture` (proto2, descriptor at file offset
/// 0x1209680) → the V1 runtime data the DSP takes. Output order (C): sample rate, pre/post lengths,
/// pre then post coefficients, pre/post gains, layer count, layer sizes, per layer weights then
/// biases, high-pass frequency then bandwidth, all LE.
///
/// Checks: stock requires about 48 kHz, lengths 512/2048 and an output of at most 0x2fff
/// bytes (C). Repeated-field counts must match their lengths and sizes (I: "checked
/// sizes/weights/biases"). Non-finite values are refused (O). More layer messages than
/// `Limits::max_layers` are refused while parsing, before their tensors are allocated.
pub fn v1_runtime_data(capture: &[u8]) -> Result<Vec<u8>, StoredError> {
    let mut scalars = [0u32; 12];
    let mut pre = Vec::new();
    let mut post = Vec::new();
    let mut sizes = Vec::new();
    let mut layers = Vec::new();
    for field in Fields(capture) {
        match field? {
            (number @ (1 | 6 | 7 | 11 | 12), Value::Fixed32(bits)) => {
                scalars[number as usize - 1] = bits;
            }
            (number @ (2 | 3 | 8), Value::Varint(value)) => {
                scalars[number as usize - 1] = u32::try_from(value).map_err(|_| Malformed)?;
            }
            (4, value) => floats(value, &mut pre)?,
            (5, value) => floats(value, &mut post)?,
            (9, Value::Varint(size)) => sizes.push(u32::try_from(size).map_err(|_| Malformed)?),
            (9, Value::Bytes(packed)) => {
                for size in Fields::packed_varints(packed) {
                    sizes.push(u32::try_from(size?).map_err(|_| Malformed)?);
                }
            }
            (10, Value::Bytes(message)) => {
                if layers.len() == Limits::default().max_layers {
                    return Err(crate::Error::Limit.into());
                }
                let mut layer = LayerTensors::default();
                for field in Fields(message) {
                    match field? {
                        (1, value) => floats(value, &mut layer.weights)?,
                        (2, value) => floats(value, &mut layer.biases)?,
                        _ => {}
                    }
                }
                layers.push(layer);
            }
            (1..=12, _) => return Err(Malformed.into()),
            _ => {}
        }
    }
    // Indices are field numbers − 1; 4, 5, 9 and 10 are repeated fields.
    let [
        rate,
        pre_len,
        post_len,
        _,
        _,
        pre_gain,
        post_gain,
        layer_count,
        _,
        _,
        hp_frequency,
        hp_bandwidth,
    ] = scalars;
    let count = |field, found: usize, expected: u32| {
        let expected = expected as usize;
        if found == expected {
            Ok(())
        } else {
            Err(StoredError::Count {
                field,
                found,
                expected,
            })
        }
    };
    count("pre_filter_coefficients", pre.len(), pre_len)?;
    count("post_filter_coefficients", post.len(), post_len)?;
    count("layer_sizes", sizes.len(), layer_count.saturating_add(1))?;
    count("weights_and_biases", layers.len(), layer_count)?;
    for (layer, pair) in layers.iter().zip(sizes.windows(2)) {
        count(
            "weights",
            layer.weights.len(),
            pair[0].saturating_mul(pair[1]),
        )?;
        count("biases", layer.biases.len(), pair[1])?;
    }

    let mut out = Vec::new();
    let mut words = |words: &[u32]| out.extend(words.iter().flat_map(|word| word.to_le_bytes()));
    words(&[rate, pre_len, post_len]);
    let bits = |values: &[f32]| {
        values
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>()
    };
    words(&bits(&pre));
    words(&bits(&post));
    words(&[pre_gain, post_gain, layer_count]);
    words(&sizes);
    for layer in &layers {
        words(&bits(&layer.weights));
        words(&bits(&layer.biases));
    }
    words(&[hp_frequency, hp_bandwidth]);
    RuntimeData::parse(&out, Limits::default())?.validate_runtime_values()?;
    Ok(out)
}

/// A repeated float field: one fixed32 per element, or packed.
fn floats(value: Value<'_>, out: &mut Vec<f32>) -> Result<(), Malformed> {
    match value {
        Value::Fixed32(bits) => out.push(f32::from_bits(bits)),
        Value::Bytes(packed) if packed.len() % 4 == 0 => out.extend(
            packed
                .chunks_exact(4)
                .map(|word| f32::from_le_bytes(word.try_into().expect("four bytes"))),
        ),
        _ => return Err(Malformed),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(field: u32, wire: u32) -> u8 {
        (field << 3 | wire) as u8
    }

    fn fixed(out: &mut Vec<u8>, field: u32, value: f32) {
        out.push(key(field, 5));
        out.extend(value.to_le_bytes());
    }

    fn varint(out: &mut Vec<u8>, field: u32, value: u32) {
        out.push(key(field, 0));
        varint_raw(out, value);
    }

    fn bytes(out: &mut Vec<u8>, field: u32, payload: &[u8]) {
        out.push(key(field, 2));
        varint_raw(out, payload.len() as u32);
        out.extend(payload);
    }

    fn varint_raw(out: &mut Vec<u8>, mut value: u32) {
        while value >= 0x80 {
            out.push(value as u8 | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    fn packed(values: impl IntoIterator<Item = f32>) -> Vec<u8> {
        values.into_iter().flat_map(f32::to_le_bytes).collect()
    }

    /// A synthetic 1→2→2→1 capture: packed coefficients and sizes, layer 0 unpacked.
    fn capture() -> Vec<u8> {
        let mut out = Vec::new();
        fixed(&mut out, 1, 48_000.0);
        varint(&mut out, 2, 512);
        varint(&mut out, 3, 2048);
        bytes(&mut out, 4, &packed((0..512).map(|i| i as f32 / 512.0)));
        bytes(&mut out, 5, &packed((0..2048).map(|i| -(i as f32))));
        fixed(&mut out, 6, 0.75);
        fixed(&mut out, 7, 0.5);
        varint(&mut out, 8, 3);
        let mut sizes = Vec::new();
        for size in [1, 2, 2, 1] {
            varint_raw(&mut sizes, size);
        }
        bytes(&mut out, 9, &sizes);
        let mut first = Vec::new();
        for weight in [1.0, 2.0] {
            fixed(&mut first, 1, weight);
        }
        bytes(&mut first, 2, &packed([0.1, 0.2]));
        bytes(&mut out, 10, &first);
        let mut middle = Vec::new();
        bytes(&mut middle, 1, &packed([3.0, 4.0, 5.0, 6.0]));
        bytes(&mut middle, 2, &packed([0.3, 0.4]));
        bytes(&mut out, 10, &middle);
        let mut last = Vec::new();
        bytes(&mut last, 1, &packed([7.0, 8.0]));
        bytes(&mut last, 2, &packed([0.9]));
        bytes(&mut out, 10, &last);
        fixed(&mut out, 11, 30.0);
        fixed(&mut out, 12, 0.7);
        out
    }

    /// The 0x6ec290 append order gives the 0x2864-byte layout `RuntimeData` reads, for both
    /// packed and unpacked repeated fields.
    #[test]
    fn v1_converter_follows_the_stock_append_order() {
        let out = v1_runtime_data(&capture()).unwrap();
        assert_eq!(out.len(), 0x2864);
        let data = RuntimeData::parse(&out, Limits::default()).unwrap();
        assert_eq!(data.sample_rate(), 48_000.0);
        assert_eq!(data.pre_coefficients().get(511), Some(511.0 / 512.0));
        assert_eq!(data.post_coefficients().get(2047), Some(-2047.0));
        assert_eq!(data.constant_gains(), [0.75, 0.5]);
        assert!(data.layer_sizes().eq([1, 2, 2, 1]));
        let layers: Vec<_> = data
            .layers()
            .map(|layer| {
                (
                    layer.weights.iter().collect(),
                    layer.biases.iter().collect(),
                )
            })
            .collect::<Vec<(Vec<f32>, Vec<f32>)>>();
        assert_eq!(layers[0], (vec![1.0, 2.0], vec![0.1, 0.2]));
        assert_eq!(layers[2], (vec![7.0, 8.0], vec![0.9]));
        assert_eq!(data.high_pass(), [30.0, 0.7]);
    }

    /// Counts that disagree with the stated lengths and the stock 48 kHz rule are refused.
    #[test]
    fn v1_converter_refuses_inconsistent_records() {
        let mut short = capture();
        varint(&mut short, 8, 2);
        assert_eq!(
            v1_runtime_data(&short),
            Err(StoredError::Count {
                field: "layer_sizes",
                found: 4,
                expected: 3
            })
        );
        let mut rate = capture();
        fixed(&mut rate, 1, 44_100.0);
        assert_eq!(
            v1_runtime_data(&rate),
            Err(StoredError::Runtime(crate::Error::SampleRate))
        );
        assert_eq!(
            v1_runtime_data(&[0x0d, 1]),
            Err(StoredError::Malformed(Malformed))
        );
        // Review w5 R25: empty layer messages stop at the limit, not after a parse of all.
        let empty_layers = [key(10, 2), 0].repeat(Limits::default().max_layers + 1);
        assert_eq!(
            v1_runtime_data(&empty_layers),
            Err(StoredError::Runtime(crate::Error::Limit))
        );
    }

    /// `version` "2" selects NC2; a missing or other version is V1 (default "1").
    #[test]
    fn metadata_version_follows_the_stock_compare() {
        let nc2 = CaptureMetadata::from_json(br#"{"name":"Plexi","version":"2"}"#).unwrap();
        assert_eq!(nc2.version, RecordVersion::Nc2);
        assert_eq!(nc2.name, "Plexi");
        let v1 = CaptureMetadata::from_json(br#"{"name":"Amp"}"#).unwrap();
        assert_eq!(v1.version, RecordVersion::V1);
        assert_eq!(
            CaptureMetadata::from_json(b"not json"),
            Err(StoredError::Metadata)
        );
    }
}
