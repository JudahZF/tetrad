//! The Cortex Control local backup file (LocalBackup 40): a JSON document whose `payload` is
//! the backup archive in base64, with the creator stamp inside it. Control sends and receives
//! the raw file text in 150 000-byte chunks (`0x17ddfc`, `0x2ff33c`); the field is a proto3
//! `string`, so the parser checks UTF-8 (`0x94678c`).
//!
//! - Write `0x47a1c8`: the keys below, `payload` as a placeholder that the writer splits on;
//!   then `base64(archive[..873])`, `base64(stamp)`, `base64(rest)`. `payload_hash` is the
//!   SHA-256 hex of the archive file (`0x6a9294`).
//! - Read: `0x30c2cc` finds `"payload": "` and the next `"`; the text without the payload is
//!   the header (`0x4080cc`: `creator_version` check `0x6eb6f0`, `creator`). `0x4072fc`
//!   decodes 1164 base64 characters into the archive, keeps the next 28 (the stamp) and
//!   decodes the rest into the archive.
//!
//! A cloud backup is the same document without the stamp: the cloud create runs the writer
//! with flag `+0x10` clear (`0x300ff4` → `0x479e70(.., 0, ..)`), so `payload` is the archive
//! in base64 (6000-byte reads, a multiple of 3, so one base64 text); the cloud restore reads
//! it with `0x4072fc(.., 0, ..)` (`0x407908`).

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::{BackupError, corrupt};

/// Stamp size (`0x47f648` pads to `0x15`).
pub const STAMP_BYTES: usize = 21;
/// Archive bytes before the stamp (`0x4072fc`: 1164 base64 characters).
const STAMP_AT: usize = 873;
/// The key marker of `0x30c2cc`.
const PAYLOAD_MARKER: &str = "\"payload\": \"";
const PLACEHOLDER: &str = "PLACEHOLDER_FOR_PAYLOAD";

/// The bytes stock puts inside a local backup payload (`0x47f648`): `1` and the device serial
/// (`0x66b55c`), each byte after a random byte (`0x47f388`), padded with random bytes to 21,
/// then encrypted with the stamp key that the DSP sends for each backup. tetrad-app builds it
/// the same way. Zeroed on drop; `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct CreatorStamp(Zeroizing<[u8; STAMP_BYTES]>);

impl CreatorStamp {
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        let mut stamp = Zeroizing::new([0; STAMP_BYTES]);
        (bytes.len() == STAMP_BYTES).then(|| {
            stamp.copy_from_slice(bytes);
            Self(stamp)
        })
    }
}

impl fmt::Debug for CreatorStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CreatorStamp(..)")
    }
}

/// The header fields of `0x47a1c8` that vary. `type` (`backup`) and `compatibility`
/// (`["quad", "native"]`) are fixed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalHeader {
    /// Device type (`0x4962ac`: `quad`, `atma`).
    pub creator: String,
    /// Firmware version (`DAT_015399b4`).
    pub creator_version: String,
    /// `QC_` and the date (`0x6695c4`).
    pub name: String,
    /// `AccountConfig.ini` user name and id (`0x2ff41c`, `0x2ff3f8`).
    pub author: String,
    pub author_id: String,
    /// Date text (`0x6695c4`).
    pub created: String,
}

/// A received local backup.
#[derive(Debug)]
pub struct LocalBackup {
    pub creator: String,
    pub creator_version: String,
    pub archive: Vec<u8>,
    /// Kept as is; Tetrad does not read it.
    pub stamp: CreatorStamp,
}

/// The backup file text: with the stamp for Cortex Control, without it for the cloud. Layout
/// I: Qt indented JSON (four spaces, `": "`, sorted keys), which matches the `0x30c2cc` marker.
pub fn encode(archive: &[u8], header: &LocalHeader, stamp: Option<&CreatorStamp>) -> String {
    let text = |s: &str| Value::String(s.to_owned());
    let fields = [
        ("author", text(&header.author)),
        ("author_id", text(&header.author_id)),
        ("compatibility", Value::from(vec!["quad", "native"])),
        ("created", text(&header.created)),
        ("creator", text(&header.creator)),
        ("creator_version", text(&header.creator_version)),
        ("name", text(&header.name)),
        ("payload", text(PLACEHOLDER)),
        ("payload_hash", text(&hex(&Sha256::digest(archive)))),
        ("type", text("backup")),
    ];
    let document: Map<String, Value> = fields
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    let mut json = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut json, formatter);
    serde::Serialize::serialize(&document, &mut serializer)
        .expect("a map of strings always serializes");
    let json = String::from_utf8(json).expect("serde_json writes UTF-8");
    let (before, after) = json
        .split_once(PLACEHOLDER)
        .expect("the placeholder is in the document");
    let mut out = String::with_capacity(json.len() + archive.len() * 4 / 3 + 64);
    out.push_str(before);
    match stamp {
        Some(stamp) => {
            let split = archive.len().min(STAMP_AT);
            STANDARD.encode_string(&archive[..split], &mut out);
            STANDARD.encode_string(*stamp.0, &mut out);
            STANDARD.encode_string(&archive[split..], &mut out);
        }
        None => STANDARD.encode_string(archive, &mut out),
    }
    out.push_str(after);
    out.push('\n');
    out
}

/// The header (the text without the payload, `0x30c2cc`) and the payload text.
fn split(text: &str) -> Result<(Map<String, Value>, &str), BackupError> {
    let start = text
        .find(PAYLOAD_MARKER)
        .ok_or_else(|| corrupt("Payload key not found!"))?
        + PAYLOAD_MARKER.len();
    let len = text[start..]
        .find('"')
        .ok_or_else(|| corrupt("Payload end not found!"))?;
    let header = [&text[..start], &text[start + len..]].concat();
    let header =
        serde_json::from_str(&header).map_err(|e| corrupt(format!("backup header: {e}")))?;
    Ok((header, &text[start..start + len]))
}

/// The header `creator` of a backup file (`0x4080cc` passes it to the restore reload as the
/// source hardware), without decoding the payload. `None` for a file without a header.
pub fn creator(text: &str) -> Option<String> {
    let (header, _) = split(text).ok()?;
    Some(header.get("creator")?.as_str()?.to_owned())
}

/// The header's `creator` and `creator_version` and the payload text; a file from a newer
/// firmware than `firmware` is refused.
fn checked<'t>(text: &'t str, firmware: &str) -> Result<(String, String, &'t str), BackupError> {
    let (header, payload) = split(text)?;
    let field = |key: &str| {
        header
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let (creator, creator_version) = (field("creator"), field("creator_version"));
    if newer(&creator_version, firmware).ok_or_else(|| corrupt("creator_version"))? {
        return Err(BackupError::Firmware {
            creator: creator_version,
            current: firmware.to_owned(),
        });
    }
    Ok((creator, creator_version, payload))
}

fn decode_part(part: &str) -> Result<Vec<u8>, BackupError> {
    STANDARD
        .decode(part)
        .map_err(|e| corrupt(format!("payload: {e}")))
}

/// Reads a Cortex Control backup file and refuses one from a newer firmware than `firmware`.
pub fn decode(text: &str, firmware: &str) -> Result<LocalBackup, BackupError> {
    let (creator, creator_version, payload) = checked(text, firmware)?;
    let (head, rest) = (payload.split_at_checked(STAMP_AT / 3 * 4))
        .and_then(|(head, rest)| Some((head, rest.split_at_checked(STAMP_BYTES.div_ceil(3) * 4)?)))
        .ok_or_else(|| corrupt("payload too short"))?;
    let mut archive = decode_part(head)?;
    let stamp = Zeroizing::new(decode_part(rest.0)?);
    archive.extend(decode_part(rest.1)?);
    Ok(LocalBackup {
        creator,
        creator_version,
        archive,
        stamp: CreatorStamp::from_slice(&stamp).ok_or_else(|| corrupt("stamp size"))?,
    })
}

/// The archive of a cloud backup file (`0x407908` → `0x4072fc(.., 0, ..)`: the whole payload
/// is base64, no stamp). O: a newer firmware is refused as for a Control file (`0x407908`
/// itself does not check it).
pub fn decode_cloud(text: &str, firmware: &str) -> Result<Vec<u8>, BackupError> {
    decode_part(checked(text, firmware)?.2)
}

/// `0x6eb6f0`: the backup's firmware is newer than this one (I: dotted numbers; `None` for
/// text that is not one).
fn newer(version: &str, current: &str) -> Option<bool> {
    let parse = |text: &str| -> Option<Vec<u32>> {
        text.split('.')
            .map(|part| part.trim().parse().ok())
            .collect()
    };
    Some(parse(version)? > parse(current)?)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(version: &str) -> LocalHeader {
        LocalHeader {
            creator: "quad".into(),
            creator_version: version.into(),
            name: "QC_test".into(),
            author: "someone".into(),
            author_id: "id".into(),
            created: "2026-10-11".into(),
        }
    }

    /// The stock layout: `"payload": "` marker, the stamp after 873 archive bytes (1164 base64
    /// characters), the archive hash; a round trip gives the archive and the stamp back, and a
    /// newer firmware is refused with the stock text.
    #[test]
    fn local_backup_file_places_the_stamp_and_round_trips() {
        let archive: Vec<u8> = (0..2000u32).map(|i| (i * 7 % 251) as u8).collect();
        let stamp = CreatorStamp::from_slice(&[0x5a; STAMP_BYTES]).unwrap();
        let text = encode(&archive, &header("4.1.0"), Some(&stamp));

        let start = text.find(PAYLOAD_MARKER).unwrap() + PAYLOAD_MARKER.len();
        let payload = &text[start..start + text[start..].find('"').unwrap()];
        assert_eq!(STANDARD.decode(&payload[1164..1192]).unwrap(), [0x5a; 21]);
        assert_eq!(
            STANDARD.decode(&payload[..1164]).unwrap(),
            archive[..STAMP_AT]
        );
        assert!(text.contains(&format!(
            "\"payload_hash\": \"{}\"",
            hex(&Sha256::digest(&archive))
        )));
        assert!(text.starts_with("{\n    \"author\": \"someone\",\n"));

        let back = decode(&text, "4.1.0").unwrap();
        assert_eq!((back.archive, back.stamp), (archive.clone(), stamp.clone()));
        assert_eq!(back.creator, "quad");
        assert_eq!(creator(&text).as_deref(), Some("quad"));

        let newer = encode(&archive, &header("4.2.0"), Some(&stamp));
        let refused = decode(&newer, "4.1.0").unwrap_err();
        assert!(
            refused
                .to_string()
                .starts_with("Backup was created with FW version 4.2.0.")
        );
        assert!(matches!(
            decode("{\"type\": \"backup\"}", "4.1.0"),
            Err(BackupError::Corrupt(reason)) if reason == "Payload key not found!"
        ));
    }

    /// The cloud file (`0x479e70(.., 0, ..)`): the same header, `payload` is the whole archive
    /// in base64 with no stamp, and the cloud read (`0x4072fc(.., 0, ..)`) gives it back.
    #[test]
    fn cloud_backup_file_has_no_stamp_and_round_trips() {
        let archive: Vec<u8> = (0..2000u32).map(|i| (i * 7 % 251) as u8).collect();
        let text = encode(&archive, &header("4.1.0"), None);

        let start = text.find(PAYLOAD_MARKER).unwrap() + PAYLOAD_MARKER.len();
        let payload = &text[start..start + text[start..].find('"').unwrap()];
        assert_eq!(payload, STANDARD.encode(&archive));
        assert!(text.contains("\"type\": \"backup\""));
        assert_eq!(decode_cloud(&text, "4.1.0").unwrap(), archive);
        let newer = encode(&archive, &header("4.2.0"), None);
        assert!(matches!(
            decode_cloud(&newer, "4.1.0"),
            Err(BackupError::Firmware { .. })
        ));
    }
}
