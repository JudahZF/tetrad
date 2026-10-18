//! The legacy capture format conversion of the stock capture manager (`0x702d58` → `0x702204` →
//! `0x70176c` → `0x6ff72c`): a `*.cns` in "My Captures" without a `<base>.json` sidecar is a
//! format-0 file. Its 0x2864-byte parameter block becomes a `captureProto.Capture` record
//! (`0x6ef594`), written as `<hash>.cns` with a new `<hash>.json` (`0x6f7a70`), both in the device
//! cipher; then the old file goes.
//!
//! O: the stock steps write in place and delete a file that does not convert. Tetrad stages
//! both files, reads them back, lets the caller journal the capture, renames them, reads them
//! again and only then removes the old file. A file that does not convert is kept.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tetrad_capture::stored::{
    CaptureMetadata, RecordVersion, device_type_name, instrument_type_name, v1_runtime_data,
};
use tetrad_capture::v1::{Limits, RuntimeData};
use tetrad_media::factory::DeviceCipher;
use tetrad_storage::media::{read_file, write_atomic};

use super::StockLibrary;
use super::media::{LEGACY_CAPTURE_DIR, base_name, legacy_files};
use crate::sidecar::utc_date;
use crate::{Error, Result};

/// The suffix of a converted file before its commit ([`StockLibrary::stage_legacy_conversion`]).
/// It exists only between a cut run and the next boot; it is never a stock file.
pub const CONVERTED_SUFFIX: &str = ".tetrad-converted";

/// `0x6fa77c`: the only parameter block size of format 0.
const PARAMS_BLOCK: usize = 0x2864;
/// `0x6ef594`: the serialized record must be 1..=0x2fff bytes.
pub(super) const MAX_RECORD: usize = 0x2fff;

/// The result of staging one legacy capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyConversion {
    /// `<hash>.cns` and `<hash>.json` are staged and read back. Journal the capture, then call
    /// [`StockLibrary::commit_legacy_conversion`].
    Staged { hash: String },
    /// The file does not convert; it stays as it is (O: stock deletes it).
    Kept(String),
}

/// The result of [`StockLibrary::commit_legacy_conversion`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyCommit {
    /// `<hash>.*` reads back as the old block and the old file is gone.
    Committed,
    /// Nothing staged and no converted pair: stage again (the old file was not touched), or,
    /// with no old file either, drop the journal entry.
    NotStaged,
    /// The pair does not verify: the staged copies are removed (and a pair this commit renamed
    /// in), the old file stays (review r25 Q1). The caller names it and goes on.
    Kept(String),
}

/// The fields of a format-0 file (`0x6fa77c`) the conversion uses.
pub(super) struct FormatZero<'a> {
    params: &'a [u8],
    /// `+0x10`, split at `|` into the author name and id (`0x6f4b38`).
    author: String,
    tags: Vec<String>,
}

/// `0x6fa77c` on decrypted bytes. All offsets are big-endian words: magic `CNS` at 0, format
/// word at 7 (0), header word `h` at 0xb, name at 0xf, author after it, then the tag count
/// and tags (each a length and bytes, read as C strings). The block size word is at `h + 7`
/// and the block at `h + 0xb`; it starts with the f32 sample rate (48 kHz, relative 1e-5).
/// O: every read is bounds-checked (stock checks only the block end).
pub(super) fn parse_format_zero(bytes: &[u8]) -> std::result::Result<FormatZero<'_>, String> {
    let word = |at: usize| -> std::result::Result<usize, String> {
        let end = at.checked_add(4).ok_or("offset overflow")?;
        let word = bytes.get(at..end).ok_or("truncated header")?;
        Ok(u32::from_be_bytes(word.try_into().expect("four bytes")) as usize)
    };
    let text = |at: usize, len: usize| -> std::result::Result<String, String> {
        let end = at.checked_add(len).ok_or("offset overflow")?;
        let raw = bytes.get(at..end).ok_or("truncated text")?;
        let raw = raw.split(|&byte| byte == 0).next().unwrap_or_default();
        Ok(String::from_utf8_lossy(raw).into_owned())
    };
    if bytes.get(..3) != Some(b"CNS") {
        return Err("not a format 0 capture (no CNS magic)".into());
    }
    let format = word(7)?;
    if format != 0 {
        return Err(format!("unknown capture file format {format}"));
    }
    let header = word(0xb)?;
    let start = header.checked_add(0xb).ok_or("offset overflow")?;
    let size = word(header.checked_add(7).ok_or("offset overflow")?)?;
    if size != PARAMS_BLOCK {
        return Err(format!("paramsBlockSize is not {PARAMS_BLOCK}: {size}"));
    }
    let params = start
        .checked_add(PARAMS_BLOCK)
        .and_then(|end| bytes.get(start..end))
        .ok_or("(byteIndex + paramsBlockSize) is out of range")?;
    let rate = f32::from_le_bytes(params[..4].try_into().expect("four bytes"));
    // A comparison that NaN fails (review r25 Q9).
    let near = (48_000.0 - rate).abs() <= rate.abs().min(48_000.0) * 1e-5;
    if !near {
        return Err(format!("samplerate = {rate}"));
    }
    let name_len = word(0xf)?;
    let author_at = 0x13usize.checked_add(name_len).ok_or("offset overflow")?;
    let author_len = word(author_at)?;
    let author = text(author_at + 4, author_len)?;
    let mut at = (author_at + 4)
        .checked_add(author_len)
        .ok_or("offset overflow")?;
    let count = word(at)?;
    at += 4;
    let mut tags = Vec::new();
    for _ in 0..count {
        let len = word(at)?;
        tags.push(text(at + 4, len)?);
        at = (at + 4).checked_add(len).ok_or("offset overflow")?;
    }
    Ok(FormatZero {
        params,
        author,
        tags,
    })
}

/// `0x6ef594`: the block as a proto3 `captureProto.Capture` (descriptor at ZenUI file offset
/// 0x1209680, `syntax = "proto3"`): repeated numbers packed, zero scalars and empty lists
/// left out, fields in number order. Protobuf 3.18 leaves out `-0.0` too (`x <= 0 && x >= 0`).
pub(super) fn capture_record(data: &RuntimeData<'_>) -> Vec<u8> {
    fn key(out: &mut Vec<u8>, field: u32, wire: u32) {
        varint(out, u64::from(field << 3 | wire));
    }
    fn varint(out: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            out.push(value as u8 | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }
    fn number(out: &mut Vec<u8>, field: u32, value: u32) {
        if value != 0 {
            key(out, field, 0);
            varint(out, u64::from(value));
        }
    }
    fn float(out: &mut Vec<u8>, field: u32, value: f32) {
        if value != 0.0 {
            key(out, field, 5);
            out.extend(value.to_le_bytes());
        }
    }
    fn bytes(out: &mut Vec<u8>, field: u32, payload: &[u8]) {
        key(out, field, 2);
        varint(out, payload.len() as u64);
        out.extend(payload);
    }
    fn packed(out: &mut Vec<u8>, field: u32, payload: &[u8]) {
        if !payload.is_empty() {
            bytes(out, field, payload);
        }
    }
    let mut out = Vec::new();
    let [pre_gain, post_gain] = data.constant_gains();
    let [hp_frequency, hp_bandwidth] = data.high_pass();
    float(&mut out, 1, data.sample_rate());
    number(&mut out, 2, data.pre_coefficients().len() as u32);
    number(&mut out, 3, data.post_coefficients().len() as u32);
    packed(&mut out, 4, data.pre_coefficients().as_bytes());
    packed(&mut out, 5, data.post_coefficients().as_bytes());
    float(&mut out, 6, pre_gain);
    float(&mut out, 7, post_gain);
    number(&mut out, 8, data.layers().count() as u32);
    let mut sizes = Vec::new();
    data.layer_sizes()
        .for_each(|size| varint(&mut sizes, u64::from(size)));
    packed(&mut out, 9, &sizes);
    for layer in data.layers() {
        let mut message = Vec::new();
        packed(&mut message, 1, layer.weights.as_bytes());
        packed(&mut message, 2, layer.biases.as_bytes());
        bytes(&mut out, 10, &message);
    }
    float(&mut out, 11, hp_frequency);
    float(&mut out, 12, hp_bandwidth);
    out
}

/// The block as the stored record loads back (`0x6ec290`): the float scalars at 0x280c,
/// 0x2810 and the two high-pass words lose a `-0.0` sign in [`capture_record`].
fn expected_runtime(params: &[u8]) -> Vec<u8> {
    let mut expected = params.to_vec();
    let end = expected.len();
    for at in [0x280c, 0x2810, end - 8, end - 4] {
        if expected[at..at + 4] == (-0.0f32).to_le_bytes() {
            expected[at..at + 4].fill(0);
        }
    }
    expected
}

/// A converted capture, before encryption.
struct Converted {
    record: Vec<u8>,
    metadata: Vec<u8>,
    /// Lowercase hex SHA-256 of the parameter block (`0x6a9204`): the new file name.
    hash: String,
    /// [`expected_runtime`] of the block.
    runtime: Vec<u8>,
}

/// `0x6ff72c` on decrypted format-0 bytes, with the metadata of `0x6f7a70(…, 1, name, dir,
/// author, hash, "", "default", "instrument_not_set", -1, "Local", tags, "", 0x2864, date)`.
fn convert(
    bytes: &[u8],
    name: &str,
    created: SystemTime,
) -> std::result::Result<Converted, String> {
    let legacy = parse_format_zero(bytes)?;
    let data = RuntimeData::parse(legacy.params, Limits::default()).map_err(|e| e.to_string())?;
    data.validate_runtime_values().map_err(|e| e.to_string())?;
    let record = capture_record(&data);
    if !(1..=MAX_RECORD).contains(&record.len()) {
        return Err(format!("record size {} is out of range", record.len()));
    }
    let hash: String = Sha256::digest(legacy.params)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let (author_name, author_id) = match legacy.author.split_once('|') {
        Some((name, rest)) => (name, rest.split('|').next().unwrap_or_default()),
        None => (legacy.author.as_str(), ""),
    };
    let text = |value: &str| Value::String(value.to_owned());
    let metadata: Map<String, Value> = [
        ("version", text("1")),
        ("name", text(name)),
        ("path", text(&format!("{LEGACY_CAPTURE_DIR}/{hash}.cns"))),
        ("authorName", text(author_name)),
        ("authorId", text(author_id)),
        ("hash", text(&hash)),
        ("cloudUUID", text("")),
        ("description", text("")),
        ("deviceType", text(device_type_name(0))),
        ("instrumentType", text(instrument_type_name(0))),
        ("parametersBlockLength", text(&PARAMS_BLOCK.to_string())),
        ("encryption", text("Local")),
        ("date", text(&utc_date(created))),
        ("tags", Value::from(legacy.tags)),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    Ok(Converted {
        record,
        metadata: serde_json::to_vec(&Value::Object(metadata)).map_err(|e| e.to_string())?,
        hash,
        runtime: expected_runtime(legacy.params),
    })
}

/// The converted pair as the library loads it: the record through `v1_runtime_data` and the
/// metadata as a version-1 object with this `hash`. Returns the record's runtime data.
fn read_converted(
    record: &Path,
    metadata: &Path,
    hash: &str,
    local: &DeviceCipher,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    let invalid = |detail: &str| Error::LegacyCapture(format!("{hash}: {detail}"));
    let read = |path: &Path| -> Result<Vec<u8>> {
        let mut bytes = read_file(path, max_bytes)?.ok_or_else(|| invalid("file missing"))?;
        local.apply(&mut bytes);
        Ok(bytes)
    };
    let json = read(metadata)?;
    let fields = CaptureMetadata::from_json(&json).map_err(|e| invalid(&e.to_string()))?;
    let stated = serde_json::from_slice::<Value>(&json)
        .ok()
        .and_then(|value| value.get("hash")?.as_str().map(str::to_owned));
    if fields.version != RecordVersion::V1 || stated.as_deref() != Some(hash) {
        return Err(invalid("metadata does not name this record"));
    }
    v1_runtime_data(&read(record)?).map_err(|e| invalid(&e.to_string()))
}

impl StockLibrary {
    /// The legacy captures the conversion (`0x702204` → `0x70176c`) works on: each `*.cns`
    /// file whose `<base name>.json` sidecar is missing or empty, sorted. The legacy import
    /// (`0x6e6c18`) skips such a capture.
    pub fn unconverted_legacy_captures(&self) -> Result<Vec<String>> {
        let Some(dir) = self.legacy_capture_dir()? else {
            return Ok(Vec::new());
        };
        let mut files = Vec::new();
        for file in legacy_files(&dir, ".cns")? {
            let sidecar = dir.join(format!("{}.json", base_name(&file)));
            if fs::metadata(&sidecar).map_or(true, |metadata| metadata.len() == 0) {
                files.push(file);
            }
        }
        Ok(files)
    }

    /// `0x70176c` → `0x6ff72c` for one file of [`Self::unconverted_legacy_captures`], first
    /// half: decrypt with `local` (`0x6abcac`), convert, then write complete
    /// `<hash>.json`[`CONVERTED_SUFFIX`] and `<hash>.cns`[`CONVERTED_SUFFIX`] copies in the
    /// device cipher (`0x6a974c`, `0x701398`) and read them back. The old file stays. The
    /// metadata `name` is the base name and `date` the directory's change time
    /// (`QFileInfo(dir).created()`, Qt 4 on Linux: `st_ctime`).
    pub fn stage_legacy_conversion(
        &self,
        file: &str,
        local: &DeviceCipher,
    ) -> Result<LegacyConversion> {
        let Some(dir) = self.legacy_capture_dir()? else {
            return Ok(LegacyConversion::Kept("no legacy capture directory".into()));
        };
        let Some(mut bytes) = read_file(&dir.join(file), self.limits.max_file_bytes)? else {
            return Ok(LegacyConversion::Kept("file missing".into()));
        };
        local.apply(&mut bytes);
        let created = {
            use std::os::unix::fs::MetadataExt;
            let seconds = fs::metadata(&dir)?.ctime().max(0) as u64;
            UNIX_EPOCH + Duration::from_secs(seconds)
        };
        let converted = match convert(&bytes, base_name(file), created) {
            Ok(converted) => converted,
            Err(reason) => return Ok(LegacyConversion::Kept(reason)),
        };
        let hash = converted.hash;
        let staged = |extension: &str| dir.join(format!("{hash}.{extension}{CONVERTED_SUFFIX}"));
        for (extension, mut plain) in [("json", converted.metadata), ("cns", converted.record)] {
            local.apply(&mut plain);
            write_atomic(&staged(extension), &plain)?;
        }
        let max = self.limits.max_file_bytes;
        let differs = match read_converted(&staged("cns"), &staged("json"), &hash, local, max) {
            Ok(runtime) if runtime == converted.runtime => None,
            Ok(_) => Some("staged record differs".to_owned()),
            Err(Error::LegacyCapture(reason)) => Some(reason),
            Err(error) => return Err(error),
        };
        if let Some(reason) = differs {
            remove_files(&dir, [staged("json"), staged("cns")])?;
            return Ok(LegacyConversion::Kept(reason));
        }
        Ok(LegacyConversion::Staged { hash })
    }

    /// The second half of [`Self::stage_legacy_conversion`], after the journal names `file` and
    /// `hash`. The pair to commit (each staged file, else `<hash>.*` after a cut between the
    /// two renames) must read as the old file's parameter block **before** any rename. Then
    /// the staged `.json`, then the staged `.cns`, go over `<hash>.*`, the pair is read again,
    /// and only then is `file` removed. Committing a committed capture again is a no-op.
    ///
    /// O (review r25 Q1, Q8): a pair that does not verify is [`LegacyCommit::Kept`], never an
    /// error that would stop every later capture at each boot. An existing `<hash>.*` pair that
    /// reads as the same block is kept with its sidecar (its name, tags and cloud id); the
    /// staged copies go (stock `0x70176c` writes over it).
    pub fn commit_legacy_conversion(
        &self,
        file: &str,
        hash: &str,
        local: &DeviceCipher,
    ) -> Result<LegacyCommit> {
        let Some(dir) = self.legacy_capture_dir()? else {
            return Ok(LegacyCommit::NotStaged);
        };
        let max = self.limits.max_file_bytes;
        let target = |extension: &str| dir.join(format!("{hash}.{extension}"));
        let staged = |extension: &str| dir.join(format!("{hash}.{extension}{CONVERTED_SUFFIX}"));
        let is_file = |path: &Path| fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
        let source = dir.join(file);
        let source_bytes = read_file(&source, max)?.filter(|_| source != target("cns"));
        let staged_any = is_file(&staged("json")) || is_file(&staged("cns"));
        if !staged_any && !target("cns").exists() && !target("json").exists() {
            return Ok(LegacyCommit::NotStaged);
        }
        let expected = match source_bytes {
            Some(mut bytes) => {
                local.apply(&mut bytes);
                match parse_format_zero(&bytes) {
                    Ok(legacy) => Some(expected_runtime(legacy.params)),
                    Err(reason) => {
                        remove_files(&dir, [staged("json"), staged("cns")])?;
                        return Ok(LegacyCommit::Kept(reason));
                    }
                }
            }
            None => None,
        };
        // `Some(reason)` when the pair does not read as the old block.
        let verify = |cns: &Path, json: &Path| -> Result<Option<String>> {
            match read_converted(cns, json, hash, local, max) {
                Ok(runtime) if expected.as_ref().is_none_or(|e| *e == runtime) => Ok(None),
                Ok(_) => Ok(Some(format!("{hash}.cns holds another capture"))),
                Err(Error::LegacyCapture(reason)) => Ok(Some(reason)),
                Err(error) => Err(error),
            }
        };
        let existing_matches =
            expected.is_some() && verify(&target("cns"), &target("json"))?.is_none();
        let renamed = staged_any && !existing_matches;
        if renamed {
            let pick = |ext: &str| match is_file(&staged(ext)) {
                true => staged(ext),
                false => target(ext),
            };
            if let Some(reason) = verify(&pick("cns"), &pick("json"))? {
                remove_files(&dir, [staged("json"), staged("cns")])?;
                return Ok(LegacyCommit::Kept(reason));
            }
            for extension in ["json", "cns"] {
                if is_file(&staged(extension)) {
                    fs::rename(staged(extension), target(extension))?;
                    fs::File::open(&dir)?.sync_all()?;
                }
            }
        } else {
            remove_files(&dir, [staged("json"), staged("cns")])?;
        }
        if let Some(reason) = verify(&target("cns"), &target("json"))? {
            // The old file stays; a pair this commit renamed in goes with the staged copies.
            if renamed && expected.is_some() {
                remove_files(&dir, [target("json"), target("cns")])?;
            }
            return Ok(LegacyCommit::Kept(reason));
        }
        if expected.is_some() {
            fs::remove_file(&source)?;
            fs::File::open(&dir)?.sync_all()?;
        }
        Ok(LegacyCommit::Committed)
    }
}

/// Removes the files that exist, then syncs `dir`.
fn remove_files<const N: usize>(dir: &Path, paths: [std::path::PathBuf; N]) -> Result<()> {
    for path in paths {
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error.into());
        }
    }
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}
