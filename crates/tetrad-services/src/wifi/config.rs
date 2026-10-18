//! `/media/p4/wpa_supplicant.conf`: saved networks and their hashed PSKs (prepare VA 0x008931b8,
//! migration 0x00898c04).

use std::path::Path;

use crate::exec::{FileError, Files};

use super::CONFIG_PATH;
use super::psk::Psk;

/// Contents written when the file is missing or empty.
pub const INITIAL: &str = "ctrl_interface=/var/run/wpa_supplicant\nupdate_config=1\n";
/// Empty marker: the config already holds hashed PSKs (string VA 0x0120cf1c).
pub const HASHED_MARKER: &str = "/media/p4/wpa_supplicant_hashed";
/// Settings backup copy used by backup/restore (VA 0x00406844).
pub const BACKUP_PATH: &str = "/media/p4/wpa_supplicant_backup.conf";

/// Create, migrate and normalize the config before the supplicant starts. Writes and syncs only
/// on change.
pub fn prepare(files: &mut impl Files) -> Result<(), FileError> {
    let path = Path::new(CONFIG_PATH);
    let old = files
        .read(path)?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .filter(|text| !text.is_empty());
    let Some(old) = old else {
        files.write_atomic(path, INITIAL.as_bytes())?;
        return files.sync();
    };
    let migrate = files.size(Path::new(HASHED_MARKER))?.is_none();
    let mut next = if migrate {
        hash_passphrases(&old)
    } else {
        old.clone()
    };
    next = normalize(&next);
    if next != old {
        files.write_atomic(path, next.as_bytes())?;
    }
    if migrate {
        files.write(Path::new(HASHED_MARKER), b"")?;
    }
    if next != old || migrate {
        files.sync()?;
    }
    Ok(())
}

/// Drop every `bssid=` line and give each `network={…}` block `disabled=1`; the worker enables
/// networks itself (`ENABLE_NETWORK all`). Per-line conditions are inferred (the decompile lost
/// the `indexOf` arguments).
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut in_block = false;
    let mut has_disabled = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("bssid=") {
            continue;
        }
        if trimmed.starts_with("network={") {
            in_block = true;
            has_disabled = false;
        } else if in_block && trimmed.starts_with("disabled=1") {
            has_disabled = true;
        } else if in_block && trimmed == "}" {
            if !has_disabled {
                out.push_str("\tdisabled=1\n");
            }
            in_block = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Replace each quoted `psk="passphrase"` with its 64-hex PBKDF2 form, salted with the block's
/// SSID. Stock strips quotes only; a hex SSID (no quotes) is decoded here (owned fix).
pub fn hash_passphrases(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::with_capacity(text.len());
    let mut block_start = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("network={") {
            block_start = Some(i);
        } else if trimmed == "}" {
            block_start = None;
        }
        let rewritten = block_start
            .and_then(|start| {
                let passphrase = quoted(trimmed.strip_prefix("psk=")?)?;
                let ssid = lines[start..]
                    .iter()
                    .take_while(|l| l.trim() != "}")
                    .find_map(|l| l.trim().strip_prefix("ssid="))
                    .and_then(ssid_bytes)?;
                let psk = Psk::derive(passphrase, &ssid)?;
                let indent = &line[..line.len() - line.trim_start().len()];
                Some(format!("{indent}psk={}", psk.hex()))
            })
            .unwrap_or_else(|| (*line).to_owned());
        out.push_str(&rewritten);
        out.push('\n');
    }
    out
}

fn quoted(value: &str) -> Option<&str> {
    value.strip_prefix('"')?.strip_suffix('"')
}

fn ssid_bytes(value: &str) -> Option<Vec<u8>> {
    if let Some(text) = quoted(value) {
        return Some(text.as_bytes().to_vec());
    }
    (value.is_ascii() && value.len().is_multiple_of(2))
        .then(|| {
            (0..value.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&value[i..i + 2], 16).ok())
                .collect()
        })
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::MemFiles;

    #[test]
    fn prepare_migrates_once_and_normalizes() {
        let mut files = MemFiles::default();
        prepare(&mut files).unwrap();
        assert_eq!(files.get(CONFIG_PATH).unwrap(), INITIAL.as_bytes());

        files.put(
            CONFIG_PATH,
            "update_config=1\nnetwork={\n\tssid=\"IEEE\"\n\tbssid=02:00:00:00:00:01\n\tpsk=\"password\"\n}\n",
        );
        files.remove(Path::new(HASHED_MARKER)).unwrap();
        prepare(&mut files).unwrap();
        assert_eq!(
            String::from_utf8(files.get(CONFIG_PATH).unwrap()).unwrap(),
            "update_config=1\nnetwork={\n\tssid=\"IEEE\"\n\tpsk=f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e\n\tdisabled=1\n}\n"
        );
        assert!(files.get(HASHED_MARKER).is_some());

        // With the marker present, a quoted psk is left alone.
        files.put(
            CONFIG_PATH,
            "network={\n\tssid=\"IEEE\"\n\tpsk=\"password\"\n\tdisabled=1\n}\n",
        );
        let syncs = files.syncs();
        prepare(&mut files).unwrap();
        assert_eq!(files.syncs(), syncs);
        assert_eq!(ssid_bytes("49454545"), Some(b"IEEE".to_vec()));
    }

    #[test]
    fn normalize_keeps_ssid_text_that_contains_bssid() {
        assert_eq!(
            normalize("network={\n\tssid=\"a bssid=x\"\n\tbssid=02:00:00:00:00:01\n}\n"),
            "network={\n\tssid=\"a bssid=x\"\n\tdisabled=1\n}\n"
        );
    }
}
