//! Read-only dump of the stock settings files found in a directory.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tetrad_settings::{SettingsFile, SettingsFileId, Variant};

use crate::{Result, io, print_json};

const MAX_SETTINGS_BYTES: usize = 1024 * 1024;
const SECRET_MARKERS: [&str; 4] = ["token", "password", "secret", "license"];

#[derive(Subcommand)]
pub enum Command {
    /// Print every key of the stock settings files in a directory. Reads only.
    Dump(Dump),
}

#[derive(Args)]
pub struct Dump {
    /// Directory holding the stock file names, such as ZenjackConfig.ini.
    #[arg(long)]
    dir: PathBuf,
}

pub fn run(command: Command) -> Result<bool> {
    match command {
        Command::Dump(args) => dump(args),
    }
}

fn dump(args: Dump) -> Result<bool> {
    let files = SettingsFileId::ALL
        .into_iter()
        .map(|id| dump_file(&args.dir, id))
        .collect::<Result<Vec<Value>>>()?;
    print_json(&json!({
        "dir": args.dir.to_string_lossy(),
        "files": files,
        "note": "Values with secret-looking key names and all PluginLicenses values are redacted"
    }))?;
    Ok(true)
}

fn dump_file(dir: &Path, id: SettingsFileId) -> Result<Value> {
    let name = Path::new(id.path())
        .file_name()
        .and_then(OsStr::to_str)
        .expect("stock settings path ends in a file name");
    let path = dir.join(name);
    if !path.is_file() {
        return Ok(json!({ "file": name, "present": false }));
    }
    let bytes = io::read_bounded(&path, MAX_SETTINGS_BYTES)?;
    let (file, issues) = SettingsFile::parse(&bytes);
    let entries: Vec<Value> = file
        .keys()
        .map(|key| describe_entry(&file, id, key))
        .collect();
    Ok(json!({
        "file": name,
        "present": true,
        "format_issues": issues.len(),
        "entries": entries
    }))
}

fn describe_entry(file: &SettingsFile, id: SettingsFileId, key: &str) -> Value {
    let (group, name) = key.rsplit_once('/').unwrap_or(("", key));
    // License values carry no recognizable key name, so the whole file is withheld.
    let redacted = id == SettingsFileId::PluginLicenses || is_secret(key);
    let value = if redacted {
        json!({ "type": "redacted" })
    } else {
        match file.value(key) {
            Some(Ok(value)) => describe_value(&value),
            Some(Err(error)) => json!({ "type": "undecodable", "error": error.to_string() }),
            None => json!({ "type": "missing" }),
        }
    };
    json!({ "group": group, "key": name, "value": value })
}

fn is_secret(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_MARKERS.iter().any(|marker| lower.contains(marker))
}

fn describe_value(value: &Variant) -> Value {
    match value {
        Variant::Invalid => json!({ "type": "invalid" }),
        Variant::Bool(v) => json!({ "type": "bool", "value": v }),
        Variant::Int(v) => json!({ "type": "int", "value": v }),
        Variant::UInt(v) => json!({ "type": "uint", "value": v }),
        Variant::LongLong(v) => json!({ "type": "longlong", "value": v }),
        Variant::ULongLong(v) => json!({ "type": "ulonglong", "value": v }),
        Variant::Double(v) => json!({ "type": "double", "value": v }),
        Variant::Float(v) => json!({ "type": "float", "value": v }),
        Variant::String(v) => json!({ "type": "string", "value": v }),
        Variant::StringList(v) => json!({ "type": "string_list", "value": v }),
        Variant::IntList(v) => json!({ "type": "int_list", "value": v }),
        Variant::ByteArray(v) => json!({ "type": "bytes", "length": v.len() }),
        other => json!({ "type": "compound", "text": format!("{other:?}") }),
    }
}
