//! Remote's file manager (docs/remote.md, "Extension files"): the asset files of the extensions
//! that build DSP blocks from them, such as NAM models. The page lists the files of each such
//! extension, sends a file in pieces and removes one. The folder is the one the extension reads
//! (`assets/<extension id>`, [`super::extension_jobs::assets_dir`]); the extension scans it again
//! after each change.

use std::path::{Path, PathBuf};

use tetrad_extensions::store::variant_ok;
use tetrad_remote::{ExtensionFiles, FileEntry, Reply};
use tetrad_services::exec::Files;

use super::extension_jobs::{ASSETS_DIR, MAX_ASSET_BYTES, MAX_ASSETS};
use super::extensions::AssetKind;

/// The folder that holds `assets/<id>`.
pub const TETRAD_DIR: &str = "/media/p4/tetrad";

/// The largest file Remote stores: what the extension reads.
pub const MAX_FILE_BYTES: u64 = MAX_ASSET_BYTES as u64;

const PART_SUFFIX: &str = ".part";

fn folder(id: &str) -> PathBuf {
    Path::new(TETRAD_DIR).join(ASSETS_DIR).join(id)
}

/// The kind with `id`, or the reason the page gets.
fn kind<'a>(kinds: &'a [AssetKind], id: &str) -> Result<&'a AssetKind, String> {
    kinds
        .iter()
        .find(|kind| kind.id == id)
        .ok_or_else(|| format!("{id} takes no files"))
}

/// A file name the extension reads: its extension is one the extension lists, and the rest is a
/// block variant name (letters, digits, spaces, `.`, `_` and `-`, 48 at most).
fn check_name(kind: &AssetKind, name: &str) -> Result<(), String> {
    let lower = name.to_ascii_lowercase();
    let Some(extension) = kind.extensions.iter().find(|e| lower.ends_with(e.as_str())) else {
        return Err(format!(
            "{} files end in {}",
            kind.label,
            kind.extensions.join(" or ")
        ));
    };
    if variant_ok(&name[..name.len() - extension.len()]) {
        Ok(())
    } else {
        Err("Use only letters, digits, spaces, '.', '_' and '-' in the name (48 characters at most, without the extension)".into())
    }
}

/// The files of every kind. `state` gives the state and the reason for a block variant of an
/// extension key.
pub fn list(
    files: &mut impl Files,
    kinds: &[AssetKind],
    state: &dyn Fn(&str, &str) -> (&'static str, String),
) -> Result<Vec<ExtensionFiles>, String> {
    let mut out = Vec::new();
    for kind in kinds {
        let dir = folder(&kind.id);
        let entries = files.list_dir(&dir).map_err(|e| e.to_string())?;
        let mut listed = Vec::new();
        for entry in entries.into_iter().flatten() {
            if entry.is_dir || check_name(kind, &entry.name).is_err() {
                continue;
            }
            let size = files
                .size(&dir.join(&entry.name))
                .map_err(|e| e.to_string())?
                .unwrap_or(0);
            let extension = kind
                .extensions
                .iter()
                .find(|e| entry.name.to_ascii_lowercase().ends_with(e.as_str()))
                .map_or(0, String::len);
            let variant = &entry.name[..entry.name.len() - extension];
            let (state, detail) = state(&kind.key, variant);
            listed.push(FileEntry {
                name: entry.name,
                size,
                state: state.into(),
                detail,
            });
        }
        out.push(ExtensionFiles {
            id: kind.id.clone(),
            name: kind.name.clone(),
            label: kind.label.clone(),
            extensions: kind.extensions.clone(),
            max_bytes: MAX_FILE_BYTES,
            files: listed,
        });
    }
    Ok(out)
}

/// One piece of a file. The piece at offset 0 starts a `.part` file; a later piece must start
/// where the stored part ends. The last piece makes the part the file, replacing a file of that
/// name. Nothing the extension reads changes before then.
pub fn chunk(
    files: &mut impl Files,
    kinds: &[AssetKind],
    id: &str,
    name: &str,
    offset: u64,
    last: bool,
    bytes: &[u8],
) -> Result<Reply, String> {
    let kind = kind(kinds, id)?;
    check_name(kind, name)?;
    let dir = folder(id);
    let path = dir.join(name);
    let part = dir.join(format!("{name}{PART_SUFFIX}"));
    let fail = |e: tetrad_services::exec::FileError| e.to_string();

    if offset == 0 {
        if files.size(&path).map_err(fail)?.is_none() {
            let present = files.list_dir(&dir).map_err(fail)?.unwrap_or_default();
            let count = present
                .iter()
                .filter(|entry| !entry.is_dir && check_name(kind, &entry.name).is_ok())
                .count();
            if count >= MAX_ASSETS {
                return Err(format!(
                    "{} takes {MAX_ASSETS} files at most; remove one first",
                    kind.name
                ));
            }
        }
        files.create_dirs(&dir).map_err(fail)?;
        files.write(&part, bytes).map_err(fail)?;
    } else {
        let have = files.size(&part).map_err(fail)?;
        if have != Some(offset) {
            return Err(format!(
                "Out of order: the unit has {} bytes of {name}, not {offset}",
                have.unwrap_or(0)
            ));
        }
        files.append(&part, bytes).map_err(fail)?;
    }
    let received = offset + bytes.len() as u64;
    if received > MAX_FILE_BYTES {
        let _ = files.remove(&part);
        return Err(format!(
            "{name} is larger than {} MiB",
            MAX_FILE_BYTES >> 20
        ));
    }
    if !last {
        return Ok(Reply::FileReceived { received });
    }
    if received == 0 {
        let _ = files.remove(&part);
        return Err(format!("{name} is empty"));
    }
    files.rename(&part, &path).map_err(fail)?;
    files.sync().map_err(fail)?;
    Ok(Reply::FileStored {
        name: name.into(),
        size: received,
    })
}

/// Removes a file (and a part of it that did not finish).
pub fn delete(
    files: &mut impl Files,
    kinds: &[AssetKind],
    id: &str,
    name: &str,
) -> Result<Reply, String> {
    let kind = kind(kinds, id)?;
    check_name(kind, name)?;
    let dir = folder(id);
    let fail = |e: tetrad_services::exec::FileError| e.to_string();
    if files.size(&dir.join(name)).map_err(fail)?.is_none() {
        return Err(format!("{name} is not on the unit"));
    }
    files.remove(&dir.join(name)).map_err(fail)?;
    files
        .remove(&dir.join(format!("{name}{PART_SUFFIX}")))
        .map_err(fail)?;
    files.sync().map_err(fail)?;
    Ok(Reply::FileDeleted { name: name.into() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetrad_services::fake::MemFiles;

    fn kinds() -> Vec<AssetKind> {
        vec![AssetKind {
            key: "JudahZF/tetrad-extensions:tetrad.nam".into(),
            id: "tetrad.nam".into(),
            name: "Neural Amp Modeler A1".into(),
            label: "NAM model".into(),
            extensions: vec![".nam".into()],
        }]
    }

    fn path(name: &str) -> String {
        format!("{TETRAD_DIR}/assets/tetrad.nam/{name}")
    }

    fn state(_: &str, variant: &str) -> (&'static str, String) {
        match variant {
            "Good" => ("ready", String::new()),
            "Bad" => ("failed", "it is an A2 model".into()),
            _ => ("waiting", String::new()),
        }
    }

    #[test]
    fn a_file_comes_in_pieces_and_replaces_the_old_one_at_the_end() {
        let mut files = MemFiles::default();
        let k = kinds();
        files.put(&path("Amp.nam"), "old");
        let a = chunk(&mut files, &k, "tetrad.nam", "Amp.nam", 0, false, b"{\"a\"").unwrap();
        assert_eq!(a, Reply::FileReceived { received: 4 });
        // The extension still reads the old file until the last piece.
        assert_eq!(files.get(&path("Amp.nam")).as_deref(), Some(&b"old"[..]));
        let b = chunk(&mut files, &k, "tetrad.nam", "Amp.nam", 4, true, b":1}").unwrap();
        assert_eq!(
            b,
            Reply::FileStored {
                name: "Amp.nam".into(),
                size: 7
            }
        );
        assert_eq!(
            files.get(&path("Amp.nam")).as_deref(),
            Some(&b"{\"a\":1}"[..])
        );
        assert!(files.get(&path("Amp.nam.part")).is_none());
    }

    #[test]
    fn a_piece_out_of_order_is_refused_and_a_new_start_begins_again() {
        let mut files = MemFiles::default();
        let k = kinds();
        chunk(&mut files, &k, "tetrad.nam", "A.nam", 0, false, b"abcd").unwrap();
        let skipped = chunk(&mut files, &k, "tetrad.nam", "A.nam", 8, false, b"x").unwrap_err();
        assert!(skipped.contains("Out of order"), "{skipped}");
        let none = chunk(&mut files, &k, "tetrad.nam", "B.nam", 4, false, b"x").unwrap_err();
        assert!(none.contains("0 bytes"), "{none}");
        // A drop and a retry from the start replaces the part.
        chunk(&mut files, &k, "tetrad.nam", "A.nam", 0, false, b"xy").unwrap();
        chunk(&mut files, &k, "tetrad.nam", "A.nam", 2, true, b"z").unwrap();
        assert_eq!(files.get(&path("A.nam")).as_deref(), Some(&b"xyz"[..]));
    }

    #[test]
    fn names_and_extensions_are_checked() {
        let mut files = MemFiles::default();
        let k = kinds();
        for (id, name) in [
            ("tetrad.other", "A.nam"),
            ("tetrad.nam", "A.wav"),
            ("tetrad.nam", "../A.nam"),
            ("tetrad.nam", "A+B.nam"),
            ("tetrad.nam", ".nam"),
            ("tetrad.nam", ".hidden.nam"),
        ] {
            assert!(
                chunk(&mut files, &k, id, name, 0, true, b"x").is_err(),
                "{id} {name}"
            );
        }
        // The extension is not case sensitive, as the scan is not.
        chunk(&mut files, &k, "tetrad.nam", "Loud.NAM", 0, true, b"x").unwrap();
        let long = format!("{}.nam", "n".repeat(49));
        assert!(chunk(&mut files, &k, "tetrad.nam", &long, 0, true, b"x").is_err());
    }

    #[test]
    fn a_file_over_the_limit_or_without_bytes_is_refused_and_leaves_no_part() {
        let mut files = MemFiles::default();
        let k = kinds();
        let big = vec![0_u8; MAX_FILE_BYTES as usize];
        chunk(&mut files, &k, "tetrad.nam", "Big.nam", 0, false, &big).unwrap();
        let over = chunk(
            &mut files,
            &k,
            "tetrad.nam",
            "Big.nam",
            MAX_FILE_BYTES,
            false,
            b"x",
        );
        assert!(over.unwrap_err().contains("larger than 3 MiB"));
        assert!(files.get(&path("Big.nam.part")).is_none());
        let empty = chunk(&mut files, &k, "tetrad.nam", "E.nam", 0, true, b"").unwrap_err();
        assert!(empty.contains("empty"), "{empty}");
        assert!(files.get(&path("E.nam.part")).is_none());
    }

    #[test]
    fn at_most_the_scan_limit_of_files() {
        let mut files = MemFiles::default();
        let k = kinds();
        for n in 0..MAX_ASSETS {
            files.put(&path(&format!("m{n}.nam")), "x");
        }
        let full = chunk(&mut files, &k, "tetrad.nam", "New.nam", 0, true, b"x").unwrap_err();
        assert!(full.contains("at most"), "{full}");
        // Replacing a file that is there is fine.
        chunk(&mut files, &k, "tetrad.nam", "m3.nam", 0, true, b"y").unwrap();
    }

    #[test]
    fn the_list_shows_sizes_and_what_the_extension_made_of_each_file() {
        let mut files = MemFiles::default();
        let k = kinds();
        files.put(&path("Good.nam"), "12345");
        files.put(&path("Bad.nam"), "1");
        files.put(&path("New.nam"), "123");
        files.put(&path("notes.txt"), "x");
        files.put(&path("Half.nam.part"), "x");
        let listed = list(&mut files, &k, &state).unwrap();
        assert_eq!(listed.len(), 1);
        let rows: Vec<_> = listed[0]
            .files
            .iter()
            .map(|f| (f.name.as_str(), f.size, f.state.as_str(), f.detail.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Bad.nam", 1, "failed", "it is an A2 model"),
                ("Good.nam", 5, "ready", ""),
                ("New.nam", 3, "waiting", ""),
            ]
        );
        assert_eq!(listed[0].extensions, [".nam"]);
    }

    #[test]
    fn delete_removes_the_file_and_its_part() {
        let mut files = MemFiles::default();
        let k = kinds();
        files.put(&path("A.nam"), "x");
        files.put(&path("A.nam.part"), "y");
        assert_eq!(
            delete(&mut files, &k, "tetrad.nam", "A.nam").unwrap(),
            Reply::FileDeleted {
                name: "A.nam".into()
            }
        );
        assert!(files.get(&path("A.nam")).is_none());
        assert!(files.get(&path("A.nam.part")).is_none());
        assert!(delete(&mut files, &k, "tetrad.nam", "A.nam").is_err());
        assert!(delete(&mut files, &k, "tetrad.nam", "../x.nam").is_err());
    }
}
