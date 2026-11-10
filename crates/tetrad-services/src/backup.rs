//! Device backups: a gzip tar of the user data under `/media/p4`, the same archive for local
//! and cloud backups.
//!
//! - Create (driver `0x4774d8`): staging `0x4763b4`, backup database pass `0x4770f4`, archive
//!   `0x6b6ab0` (libarchive gzip + pax-restricted; files `0x6b6920`).
//! - Restore (`0x406844`): header check `0x6b44e4`, clear, Wi-Fi config save, extract
//!   `0x6b4608` under `/media/p4`, Wi-Fi config back.
//!
//! All file access goes through [`Files`]. A create holds the archive in memory once; a restore
//! reads the archive and writes each member in pieces (owned).

use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use thiserror::Error;

use crate::exec::{FileError, Files};

pub mod local;

/// The data root (`DAT_01549fc4`, `DAT_0154b124`).
pub const DATA_ROOT: &str = "/media/p4";
/// Where a cloud backup lands before restore (`DAT_01549fcc`).
pub const DOWNLOADED_BACKUP: &str = "/media/p4/downloaded_backup.tar.gz";
/// The supplicant config, kept across a restore (`DAT_01549fd8`, saved copy `DAT_01549fd4`).
const WIFI_CONFIG: &str = "wpa_supplicant.conf";
const WIFI_CONFIG_SAVED: &str = "wpa_supplicant_backup.conf";

/// One archive member root.
#[derive(Debug, Clone, Copy)]
enum Member {
    /// Every file and directory below it; the root itself has no entry (`QDirIterator`
    /// with `NoDotAndDotDot`, `0x6b6ab0`).
    Tree(&'static str),
    File(&'static str),
}

/// What a backup holds, in archive order. Stock stages `Presets`, `Plugins`,
/// `Captures/setlists` and six files into `BackupTmpDir` (`0x4763b4`), then archives
/// `./Presets ./Captures ./CustomIRs ./Plugins` and eight files from there (`0x4774d8`). The
/// unstaged names (`CustomIRs`, `user_ir_folders`, `user_nc_folders`) are never in a stock
/// backup. Tetrad archives the staged set straight from the data root (O: no staging copy).
const MEMBERS: [Member; 9] = [
    Member::Tree("Presets"),
    Member::File("Captures/setlists"),
    Member::Tree("Plugins"),
    Member::File("ZenjackConfig.ini"),
    Member::File("zenui.db"),
    Member::File("user_preset_folders"),
    Member::File("favorites"),
    Member::File("favorites-nc"),
    Member::File("favorites-ir"),
];

/// Removed before an extract (`0x406844`: `0x66872c` on four trees, `QFile::remove` on three).
const RESTORE_CLEARS_TREES: [&str; 4] = ["Presets", "Captures", "CustomIRs", "Plugins"];
const RESTORE_CLEARS_FILES: [&str; 3] = ["favorites", "favorites-nc", "favorites-ir"];
/// The top-level files a [`Scope::BackupSet`] restore takes: the [`MEMBERS`] files and the two
/// unstaged folder files of the stock list (`0x4774d8`).
const BACKUP_SET_FILES: [&str; 8] = [
    "ZenjackConfig.ini",
    "zenui.db",
    "user_preset_folders",
    "user_ir_folders",
    "user_nc_folders",
    "favorites",
    "favorites-nc",
    "favorites-ir",
];

/// Which members a [`restore`] takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every safe member under the data root, as stock (cloud and Cortex Control backups).
    Any,
    /// Only the backup set: the four trees that the restore clears and the top-level files of
    /// a stock backup (`zenui.db`, `ZenjackConfig.ini`, the folder and favourites files) (O). For an archive from the Remote page, which nothing signs: any other member (the
    /// Tetrad folder with its launcher, the account files) fails the check before anything is
    /// removed.
    BackupSet,
}

impl Scope {
    fn takes(self, path: &Path) -> bool {
        let mut parts = path.iter().filter_map(|part| part.to_str());
        match (self, parts.next()) {
            (Self::Any, _) => true,
            (Self::BackupSet, Some(first)) if RESTORE_CLEARS_TREES.contains(&first) => true,
            (Self::BackupSet, Some(first)) => {
                parts.next().is_none() && BACKUP_SET_FILES.contains(&first)
            }
            (Self::BackupSet, None) => false,
        }
    }
}

const BLOCK: usize = 512;
/// libarchive's default record: the archive is padded to whole records.
const RECORD: usize = 20 * BLOCK;
/// Largest member a restore takes (O): a sanity bound on a size field; no member is held in
/// memory.
const MEMBER_LIMIT: u64 = 512 << 20;
/// Largest pax or long-name record (O): names and attributes, never file data.
const RECORD_DATA_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BackupError {
    /// "Downloaded backup is empty!" (`0x406844`, argument 0).
    #[error("Failed to download backup. Please check your internet connection.")]
    Empty,
    /// The header pass `0x6b44e4` failed, or (owned) a member is unsafe to extract.
    #[error("Downloaded backup is corrupt: {0}")]
    Corrupt(String),
    /// `0x6b4608` failed part way; some members may be written.
    #[error("Failed to extract backup: {0}")]
    Extract(FileError),
    #[error(transparent)]
    File(#[from] FileError),
    #[error("backup member {0:?} cannot be archived")]
    Member(String),
    /// The staged copy of `zenui.db` could not be made (O: the create packs nothing rather
    /// than the live file, which the device can write during the pack; review r19 F5).
    #[error("the zenui.db copy failed: {0}")]
    Stage(String),
    /// `0x4080cc` after `0x6eb6f0`.
    #[error(
        "Backup was created with FW version {creator}. The current FW version is {current}. \
         Please update this device to download the backup."
    )]
    Firmware { creator: String, current: String },
}

fn corrupt(reason: impl Into<String>) -> BackupError {
    BackupError::Corrupt(reason.into())
}

/// What [`create`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub files: usize,
    pub dirs: usize,
    pub bytes: u64,
}

/// Builds the backup archive from the data under `root` (`/media/p4` on the device) into
/// `dest`. Missing members are skipped, as stock `isFile` (`0x6b6ab0`). Each file is a
/// regular `0644` entry with its size and no time (`0x6b6920`); directories are `0755` (O:
/// stock copies their `stat`).
///
/// `./zenui.db` is read from `database`: the staged copy whose Neural Capture rows the caller
/// re-keyed (`0x4770f4`: `0x6a6358`, `0x6a68f4` with flag 0), or the live file.
pub fn create(
    files: &mut impl Files,
    root: &Path,
    database: &Path,
    dest: &Path,
) -> Result<Summary, BackupError> {
    let mut tar = TarWriter::new(GzEncoder::new(Vec::new(), Compression::default()));
    let mut summary = Summary {
        files: 0,
        dirs: 0,
        bytes: 0,
    };
    for member in MEMBERS {
        match member {
            Member::Tree(name) => {
                add_tree(files, &mut tar, &mut summary, &root.join(name), name)?;
            }
            Member::File(name) => {
                let path = match name {
                    "zenui.db" => database.to_owned(),
                    _ => root.join(name),
                };
                if let Some(data) = files.read(&path)? {
                    add_file(&mut tar, &mut summary, name, &data)?;
                }
            }
        }
    }
    let archive = tar
        .finish()
        .and_then(GzEncoder::finish)
        .map_err(|e| BackupError::Member(e.to_string()))?;
    files.write(dest, &archive)?;
    files.sync()?;
    Ok(summary)
}

/// An in-memory gzip tar of `members` (`0x6b4b50`, `0x6b4c8c`): each a regular `0644` entry
/// named as given, with its size and no time (`0x6b440c`). A preset upload packs one
/// `payload.pb` (`0x6b4c10`); a capture upload packs `<id>.cns` and `<id>.json` (`0x725ba4`).
pub fn pack(members: &[(&str, &[u8])]) -> Result<Vec<u8>, BackupError> {
    let mut packer = Packer::default();
    for (name, data) in members {
        packer.add(name, data)?;
    }
    packer.finish()
}

/// [`pack`] one member at a time, so the caller holds one plain member, not all of them.
pub struct Packer(TarWriter<GzEncoder<Vec<u8>>>);

impl Default for Packer {
    fn default() -> Self {
        Self(TarWriter::new(GzEncoder::new(
            Vec::new(),
            Compression::default(),
        )))
    }
}

impl Packer {
    pub fn add(&mut self, name: &str, data: &[u8]) -> Result<(), BackupError> {
        self.0.file(name, data)
    }

    pub fn finish(self) -> Result<Vec<u8>, BackupError> {
        (self.0.finish())
            .and_then(GzEncoder::finish)
            .map_err(|e| BackupError::Member(e.to_string()))
    }
}

fn add_file<W: Write>(
    tar: &mut TarWriter<W>,
    summary: &mut Summary,
    name: &str,
    data: &[u8],
) -> Result<(), BackupError> {
    tar.file(&format!("./{name}"), data)?;
    summary.files += 1;
    summary.bytes += data.len() as u64;
    Ok(())
}

/// Pre-order walk: a directory entry, then its members, by name (O: stock order is the
/// file system's).
fn add_tree<W: Write>(
    files: &mut impl Files,
    tar: &mut TarWriter<W>,
    summary: &mut Summary,
    dir: &Path,
    name: &str,
) -> Result<(), BackupError> {
    for entry in files.list_dir(dir)?.unwrap_or_default() {
        let path = dir.join(&entry.name);
        let member = format!("{name}/{}", entry.name);
        if entry.is_dir {
            tar.dir(&format!("./{member}"))?;
            summary.dirs += 1;
            add_tree(files, tar, summary, &path, &member)?;
        } else if let Some(data) = files.read(&path)? {
            add_file(tar, summary, &member, &data)?;
        }
    }
    Ok(())
}

/// A cloud backup body is the archive itself, or its base64 text (stock logs "Backup base64
/// decoded", `0x4072fc`; I: which transport sends which). `Some` with the archive for a
/// base64 body; `None` when the body is to be used as it is.
pub fn decode_download(body: &[u8]) -> Option<Vec<u8>> {
    if body.starts_with(&[0x1f, 0x8b]) {
        return None;
    }
    let text = body.trim_ascii();
    let text = text
        .strip_prefix(b"\"")
        .and_then(|t| t.strip_suffix(b"\""))
        .unwrap_or(text);
    STANDARD.decode(text).ok()
}

/// The size of one archive read and of one member write in a restore.
const PIECE: u64 = 64 * 1024;

/// Reads a file in pieces through [`Files::read_at`], so a restore never holds the archive
/// (O): a backup can be hundreds of MiB, and the unit has little RAM.
struct PieceReader<'a, 'f, F: Files + ?Sized> {
    files: &'a RefCell<&'f mut F>,
    path: &'a Path,
    offset: u64,
    piece: Vec<u8>,
    at: usize,
}

impl<F: Files + ?Sized> Read for PieceReader<'_, '_, F> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at == self.piece.len() {
            self.piece = (self.files.borrow_mut())
                .read_at(self.path, self.offset, PIECE)
                .map_err(|error| io::Error::other(error.to_string()))?
                .unwrap_or_default();
            self.offset += self.piece.len() as u64;
            self.at = 0;
        }
        let count = out.len().min(self.piece.len() - self.at);
        out[..count].copy_from_slice(&self.piece[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}

/// Restores the backup at `archive` under `root` (`0x406844`): header check, the four trees
/// and three favourites files removed, the Wi-Fi config saved, extract, the Wi-Fi config put
/// back. Returns the number of members written. A member outside `scope` fails the check.
///
/// `archive` is read again after the clear, so it must not be inside a tree that the restore
/// removes.
///
/// The caller closes the media library first (`0x69daac` closes the default database before
/// the extract) and reloads after it (`0x3fcf64`; tetrad-app restarts instead after a good
/// extract). Members land through a rename, so an open database keeps its old file (O).
///
/// Stock extracts with options `0x66` (no `SECURE_NODOTDOT`, no `SECURE_SYMLINKS`). Tetrad
/// refuses absolute paths, `..`, links and special files during the check, before anything
/// is removed (O).
pub fn restore(
    files: &mut impl Files,
    root: &Path,
    archive: &Path,
    scope: Scope,
) -> Result<usize, BackupError> {
    if files.size(archive)?.unwrap_or(0) == 0 {
        return Err(BackupError::Empty);
    }
    let files = RefCell::new(files);
    let open = || PieceReader {
        files: &files,
        path: archive,
        offset: 0,
        piece: Vec::new(),
        at: 0,
    };
    let mut handle = &files;
    read_members(open(), |member, _| match scope.takes(&member.path) {
        true => Ok(()),
        false => Err(corrupt(format!(
            "{} is not in a Quad Cortex backup",
            member.path.display()
        ))),
    })?;

    for name in RESTORE_CLEARS_TREES {
        handle.remove_tree(&root.join(name))?;
    }
    for name in RESTORE_CLEARS_FILES {
        handle.remove(&root.join(name))?;
    }
    let (config, saved) = (root.join(WIFI_CONFIG), root.join(WIFI_CONFIG_SAVED));
    handle.remove(&saved)?;
    if let Some(conf) = handle.read(&config)? {
        handle.write(&saved, &conf)?;
    }
    handle.sync()?;

    let mut written = 0;
    read_members(open(), |member, body| {
        extract(&mut handle, root, member, body)?;
        written += 1;
        Ok(())
    })?;
    handle.sync()?;

    if let (Some(conf), true) = (handle.read(&saved)?, handle.size(&config)?.is_some()) {
        handle.remove(&config)?;
        handle.sync()?;
        handle.write(&config, &conf)?;
        handle.remove(&saved)?;
        handle.sync()?;
    }
    Ok(written)
}

fn extract(
    files: &mut impl Files,
    root: &Path,
    member: &TarMember,
    body: &mut dyn Read,
) -> Result<(), BackupError> {
    let path = root.join(&member.path);
    match member.kind {
        Kind::Dir => files.create_dirs(&path).map_err(BackupError::Extract),
        Kind::File => {
            if let Some(parent) = path.parent() {
                files.create_dirs(parent).map_err(BackupError::Extract)?;
            }
            let mut temp = path.clone().into_os_string();
            temp.push(".restore");
            let temp = PathBuf::from(temp);
            files.write(&temp, &[]).map_err(BackupError::Extract)?;
            let mut piece = vec![0; PIECE as usize];
            loop {
                let count = body.read(&mut piece).map_err(|e| corrupt(e.to_string()))?;
                if count == 0 {
                    break;
                }
                files
                    .append(&temp, &piece[..count])
                    .map_err(BackupError::Extract)?;
            }
            files.rename(&temp, &path).map_err(BackupError::Extract)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
}

/// One checked member: `path` is relative, without `.` or `..` components. [`read_tar`] gives
/// its data as a reader beside it.
#[derive(Debug)]
struct TarMember {
    path: PathBuf,
    kind: Kind,
}

/// ustar writer with pax `path` records for long names (libarchive `pax_restricted`).
struct TarWriter<W: Write> {
    out: W,
    written: usize,
}

impl<W: Write> TarWriter<W> {
    fn new(out: W) -> Self {
        Self { out, written: 0 }
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.written += bytes.len();
        let pad = bytes.len().next_multiple_of(BLOCK) - bytes.len();
        self.out.write_all(&[0; BLOCK][..pad])?;
        self.written += pad;
        Ok(())
    }

    fn entry(&mut self, path: &str, kind: u8, mode: u32, data: &[u8]) -> Result<(), BackupError> {
        let failed = |e: io::Error| BackupError::Member(format!("{path}: {e}"));
        let (prefix, name) = match split_name(path) {
            Some(split) => split,
            None => {
                let record = pax_record("path", path);
                let header = header("", "././@PaxHeader", b'x', 0o644, record.len() as u64)
                    .ok_or_else(|| BackupError::Member(path.into()))?;
                self.put(&header).map_err(failed)?;
                self.put(record.as_bytes()).map_err(failed)?;
                ("", truncate(path, 100))
            }
        };
        let header = header(prefix, name, kind, mode, data.len() as u64)
            .ok_or_else(|| BackupError::Member(path.into()))?;
        self.put(&header).map_err(failed)?;
        if !data.is_empty() {
            self.put(data).map_err(failed)?;
        }
        Ok(())
    }

    fn file(&mut self, path: &str, data: &[u8]) -> Result<(), BackupError> {
        self.entry(path, b'0', 0o644, data)
    }

    fn dir(&mut self, path: &str) -> Result<(), BackupError> {
        self.entry(path, b'5', 0o755, &[])
    }

    /// Two zero blocks, then zeros to a whole record.
    fn finish(mut self) -> io::Result<W> {
        let end = (self.written + 2 * BLOCK).next_multiple_of(RECORD);
        self.out.write_all(&vec![0; end - self.written])?;
        Ok(self.out)
    }
}

/// The longest prefix of `text` within `max` bytes that ends on a character boundary.
fn truncate(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// ustar `prefix` (155) and `name` (100), split at a `/`; `None` when no split fits.
fn split_name(path: &str) -> Option<(&str, &str)> {
    if path.len() <= 100 {
        return Some(("", path));
    }
    path.match_indices('/')
        .map(|(at, _)| (&path[..at], &path[at + 1..]))
        .find(|(prefix, name)| prefix.len() <= 155 && !name.is_empty() && name.len() <= 100)
}

/// `"<len> key=value\n"`, where `len` counts the whole record with its own digits.
fn pax_record(key: &str, value: &str) -> String {
    let body = format!(" {key}={value}\n");
    let mut len = body.len() + 1;
    while len.to_string().len() + body.len() != len {
        len = len.to_string().len() + body.len();
    }
    format!("{len}{body}")
}

/// Octal field of `width` bytes: digits, then NUL. `None` when the value does not fit.
fn octal(field: &mut [u8], value: u64) -> Option<()> {
    let text = format!("{value:0width$o}", width = field.len() - 1);
    (text.len() < field.len()).then(|| {
        field[..text.len()].copy_from_slice(text.as_bytes());
        field[text.len()] = 0;
    })
}

fn header(prefix: &str, name: &str, kind: u8, mode: u32, size: u64) -> Option<[u8; BLOCK]> {
    if name.len() > 100 || prefix.len() > 155 {
        return None;
    }
    let mut h = [0u8; BLOCK];
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], u64::from(mode))?;
    octal(&mut h[108..116], 0)?;
    octal(&mut h[116..124], 0)?;
    octal(&mut h[124..136], size)?;
    octal(&mut h[136..148], 0)?;
    h[156] = kind;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    h[148..156].fill(b' ');
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    let text = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(text.as_bytes());
    Some(h)
}

fn field_text(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    &field[..end]
}

fn parse_octal(field: &[u8]) -> Result<u64, BackupError> {
    let text = std::str::from_utf8(field_text(field))
        .map_err(|_| corrupt("bad number"))?
        .trim_matches(' ');
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).map_err(|_| corrupt("bad number"))
}

/// `true` for the header's own checksum: unsigned or signed sum, as readers accept both.
fn checksum_ok(h: &[u8; BLOCK]) -> Result<bool, BackupError> {
    let stored = parse_octal(&h[148..156])?;
    let (mut unsigned, mut signed) = (0u64, 0i64);
    for (at, &b) in h.iter().enumerate() {
        let b = if (148..156).contains(&at) { b' ' } else { b };
        unsigned += u64::from(b);
        signed += i64::from(b as i8);
    }
    Ok(stored == unsigned || i64::try_from(stored).is_ok_and(|stored| stored == signed))
}

/// `path` relative to the extract root, `./` dropped. Absolute paths and `..` are refused.
fn member_path(raw: &str) -> Result<PathBuf, BackupError> {
    let mut path = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => return Err(corrupt(format!("unsafe member path {raw:?}"))),
        }
    }
    if path.as_os_str().is_empty() {
        return Err(corrupt("empty member path"));
    }
    Ok(path)
}

/// The `path` value of pax records, if any.
fn pax_path(records: &[u8]) -> Result<Option<String>, BackupError> {
    let mut rest = records;
    let mut path = None;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| corrupt("bad pax record"))?;
        let len: usize = std::str::from_utf8(&rest[..space])
            .ok()
            .and_then(|n| n.parse().ok())
            .filter(|&len| len > space && len <= rest.len())
            .ok_or_else(|| corrupt("bad pax record"))?;
        let record = &rest[space + 1..len];
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(value) = record.strip_prefix(b"path=") {
            path = Some(
                String::from_utf8(value.to_vec()).map_err(|_| corrupt("pax path is not UTF-8"))?,
            );
        }
        rest = &rest[len..];
    }
    Ok(path)
}

fn read_exact_or_corrupt(input: &mut impl Read, buf: &mut [u8]) -> Result<(), BackupError> {
    input
        .read_exact(buf)
        .map_err(|e| corrupt(format!("truncated archive: {e}")))
}

/// Data of `size` bytes (at most `limit`), then the padding to the next block.
fn read_data(input: &mut impl Read, size: u64, limit: u64) -> Result<Vec<u8>, BackupError> {
    if size > limit {
        return Err(corrupt(format!("member of {size} bytes")));
    }
    let mut data = Vec::new();
    input
        .by_ref()
        .take(size)
        .read_to_end(&mut data)
        .map_err(|e| corrupt(e.to_string()))?;
    if data.len() as u64 != size {
        return Err(corrupt("truncated member"));
    }
    let pad = (size as usize).next_multiple_of(BLOCK) - size as usize;
    read_exact_or_corrupt(input, &mut [0; BLOCK][..pad])?;
    Ok(data)
}

/// The first `files` regular files of an in-memory archive in order, as (path as stored,
/// data): the reads of `0x6b4d88` and `0x6b5bc4` (libarchive, every filter and format, every
/// member). O: gzip or plain tar only; directories are skipped and an absolute or `..` path is
/// refused; reading stops after `files` files, and at most `limit` bytes come out of the
/// decoder in all (review r26 S3: a gzip bomb or a large unused member cannot fill memory).
pub fn unpack(
    archive: &[u8],
    files: usize,
    limit: u64,
) -> Result<Vec<(String, Vec<u8>)>, BackupError> {
    let mut found = Vec::new();
    let visit = |member: &TarMember, body: &mut dyn Read| {
        if member.kind == Kind::File {
            let mut data = Vec::new();
            body.read_to_end(&mut data)
                .map_err(|e| corrupt(e.to_string()))?;
            found.push((member.path.display().to_string(), data));
        }
        Ok(match found.len() < files {
            true => ControlFlow::Continue(()),
            false => ControlFlow::Break(()),
        })
    };
    let input: Box<dyn Read + '_> = match archive.starts_with(&[0x1f, 0x8b]) {
        true => Box::new(GzDecoder::new(archive)),
        false => Box::new(archive),
    };
    read_tar(input.take(limit), limit, visit)?;
    Ok(found)
}

/// Reads every member of a gzip tar in order: the header pass of `0x6b44e4` plus the
/// owned path and type checks. The gzip trailer (CRC, length) is checked at the end.
fn read_members(
    archive: impl Read,
    mut visit: impl FnMut(&TarMember, &mut dyn Read) -> Result<(), BackupError>,
) -> Result<(), BackupError> {
    read_tar(GzDecoder::new(archive), MEMBER_LIMIT, |member, body| {
        visit(member, body).map(ControlFlow::Continue)
    })
}

/// The members in order, each at most `limit` bytes. `visit` gets each member with a reader
/// over its data; what it leaves unread is skipped. A `Break` from `visit` ends the read there
/// (the rest and the gzip trailer are not read).
fn read_tar(
    mut input: impl Read,
    limit: u64,
    mut visit: impl FnMut(&TarMember, &mut dyn Read) -> Result<ControlFlow<()>, BackupError>,
) -> Result<(), BackupError> {
    let mut long_path: Option<String> = None;
    loop {
        let mut h = [0u8; BLOCK];
        read_exact_or_corrupt(&mut input, &mut h)?;
        if h.iter().all(|&b| b == 0) {
            break;
        }
        if !checksum_ok(&h)? {
            return Err(corrupt("header checksum mismatch"));
        }
        let size = parse_octal(&h[124..136])?;
        let kind = h[156];
        match kind {
            b'x' => {
                long_path = pax_path(&read_data(&mut input, size, RECORD_DATA_LIMIT.min(limit))?)?
                    .or(long_path);
                continue;
            }
            b'g' => {
                read_data(&mut input, size, RECORD_DATA_LIMIT.min(limit))?;
                continue;
            }
            b'L' => {
                let name = read_data(&mut input, size, RECORD_DATA_LIMIT.min(limit))?;
                long_path = Some(
                    String::from_utf8(field_text(&name).to_vec())
                        .map_err(|_| corrupt("long name is not UTF-8"))?,
                );
                continue;
            }
            _ => {}
        }
        let raw = match long_path.take() {
            Some(path) => path,
            None => {
                let name = field_text(&h[..100]);
                let prefix = field_text(&h[345..500]);
                let joined = if h[257..262] == *b"ustar" && !prefix.is_empty() {
                    [prefix, b"/", name].concat()
                } else {
                    name.to_vec()
                };
                String::from_utf8(joined).map_err(|_| corrupt("member name is not UTF-8"))?
            }
        };
        let path = member_path(&raw)?;
        let kind = match kind {
            b'0' | 0 => Kind::File,
            b'5' => Kind::Dir,
            other => {
                return Err(corrupt(format!(
                    "member {raw:?} has type {:?}",
                    char::from(other)
                )));
            }
        };
        if size > limit {
            return Err(corrupt(format!("member of {size} bytes")));
        }
        let member = TarMember { path, kind };
        let mut body = input.by_ref().take(size);
        let flow = visit(&member, &mut body)?;
        // What the visitor did not read, then the padding to the next block.
        io::copy(&mut body, &mut io::sink()).map_err(|e| corrupt(e.to_string()))?;
        if body.limit() != 0 {
            return Err(corrupt("truncated member"));
        }
        let block = BLOCK as u64;
        let pad = ((block - size % block) % block) as usize;
        read_exact_or_corrupt(&mut input, &mut [0; BLOCK][..pad])?;
        if flow.is_break() {
            return Ok(());
        }
    }
    // The rest is padding; reading it to the end checks the gzip trailer.
    io::copy(&mut input, &mut io::sink()).map_err(|e| corrupt(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::MemFiles;

    fn root() -> &'static Path {
        Path::new(DATA_ROOT)
    }

    fn device_data() -> MemFiles {
        let files = MemFiles::default();
        files.put("/media/p4/Presets/setlists", "<setlists/>");
        files.put("/media/p4/Presets/User/a.pb", vec![1u8, 2, 3]);
        files.put("/media/p4/Plugins/p.bin", vec![7u8; 700]);
        files.put("/media/p4/Captures/setlists", "captures");
        files.put("/media/p4/Captures/input_capture.bin", vec![9u8; 64]);
        files.put("/media/p4/CustomIRs/ir.wav", "ir");
        files.put("/media/p4/zenui.db", "db");
        files.put("/media/p4/favorites", "fav");
        files.put("/media/p4/logs/zenui.log", "log");
        files
    }

    /// The member set and names of `0x4763b4`/`0x4774d8` (staged set only, `./` names, a
    /// directory entry for each subdirectory), and a restore that clears the four trees and
    /// rebuilds them from the archive while the Wi-Fi config survives.
    #[test]
    fn create_then_restore_round_trips_the_staged_set() {
        let mut files = device_data();
        let dest = Path::new("/media/p4/backup.tar.gz");
        let summary = create(&mut files, root(), &root().join("zenui.db"), dest).unwrap();
        assert_eq!((summary.files, summary.dirs), (6, 1));

        let archive = files.get("/media/p4/backup.tar.gz").unwrap();
        let mut names = Vec::new();
        read_members(&archive[..], |m, _| {
            names.push(format!(
                "{}{}",
                m.path.display(),
                if m.kind == Kind::Dir { "/" } else { "" }
            ));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            names,
            [
                "Presets/User/",
                "Presets/User/a.pb",
                "Presets/setlists",
                "Captures/setlists",
                "Plugins/p.bin",
                "zenui.db",
                "favorites",
            ]
        );

        files.put(DOWNLOADED_BACKUP, archive);
        files.put("/media/p4/Presets/User/b.pb", "new since backup");
        files.put("/media/p4/favorites-nc", "x");
        files.put("/media/p4/wpa_supplicant.conf", "network={}");
        assert_eq!(
            restore(&mut files, root(), Path::new(DOWNLOADED_BACKUP), Scope::Any),
            Ok(7)
        );
        assert_eq!(files.get("/media/p4/Presets/User/a.pb").unwrap(), [1, 2, 3]);
        assert_eq!(
            files.get("/media/p4/Plugins/p.bin").unwrap(),
            vec![7u8; 700]
        );
        for gone in [
            "/media/p4/Presets/User/b.pb",
            "/media/p4/favorites-nc",
            "/media/p4/CustomIRs/ir.wav",
            "/media/p4/Captures/input_capture.bin",
            "/media/p4/wpa_supplicant_backup.conf",
        ] {
            assert!(files.get(gone).is_none(), "{gone}");
        }
        assert_eq!(
            files.get("/media/p4/wpa_supplicant.conf").unwrap(),
            b"network={}"
        );
        assert_eq!(files.get("/media/p4/logs/zenui.log").unwrap(), b"log");
    }

    /// A member larger than one read piece and one write piece comes back byte for byte.
    #[test]
    fn a_large_member_restores_in_pieces() {
        let mut files = device_data();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        files.put("/media/p4/Plugins/big.bin", big.clone());
        let path = Path::new(DOWNLOADED_BACKUP);
        create(&mut files, root(), &root().join("zenui.db"), path).unwrap();
        files.put("/media/p4/Plugins/big.bin", "changed since backup");
        assert_eq!(restore(&mut files, root(), path, Scope::Any), Ok(8));
        assert_eq!(files.get("/media/p4/Plugins/big.bin"), Some(big));
    }

    /// An empty download, a broken archive and an unsafe member all fail before anything is
    /// removed (stock `0x6b44e4` runs first; the path rules are owned).
    #[test]
    fn restore_refuses_bad_archives_before_clearing() {
        let mut files = device_data();
        let path = Path::new(DOWNLOADED_BACKUP);
        assert_eq!(
            restore(&mut files, root(), path, Scope::Any),
            Err(BackupError::Empty)
        );

        let mut tar = TarWriter::new(GzEncoder::new(Vec::new(), Compression::default()));
        tar.file("./Presets/../../etc/passwd", b"x").unwrap();
        let hostile = tar.finish().unwrap().finish().unwrap();
        files.put(DOWNLOADED_BACKUP, hostile.clone());
        assert!(matches!(
            restore(&mut files, root(), path, Scope::Any),
            Err(BackupError::Corrupt(_))
        ));

        let mut cut = hostile;
        cut.truncate(cut.len() / 2);
        files.put(DOWNLOADED_BACKUP, cut);
        assert!(matches!(
            restore(&mut files, root(), path, Scope::Any),
            Err(BackupError::Corrupt(_))
        ));
        assert!(files.get("/media/p4/Presets/User/a.pb").is_some());
    }

    /// A [`Scope::BackupSet`] restore takes what [`create`] packs, and refuses a member outside
    /// the backup set (here a launcher script) before anything is removed; [`Scope::Any`]
    /// takes it, as stock does.
    #[test]
    fn a_backup_set_restore_takes_only_the_backup_members() {
        let mut files = device_data();
        let path = Path::new(DOWNLOADED_BACKUP);
        create(&mut files, root(), &root().join("zenui.db"), path).unwrap();
        assert_eq!(restore(&mut files, root(), path, Scope::BackupSet), Ok(7));

        let script = "./tetrad/launcher/tetrad-launch.sh";
        let archive = pack(&[
            ("./Presets/setlists", b"<setlists/>"),
            (script, b"#!/bin/sh"),
        ]);
        files.put(DOWNLOADED_BACKUP, archive.unwrap());
        assert!(matches!(
            restore(&mut files, root(), path, Scope::BackupSet),
            Err(BackupError::Corrupt(_))
        ));
        assert!(files.get("/media/p4/Presets/User/a.pb").is_some());
        assert_eq!(restore(&mut files, root(), path, Scope::Any), Ok(2));
        assert!(
            files
                .get("/media/p4/tetrad/launcher/tetrad-launch.sh")
                .is_some()
        );
    }

    /// Review r26 S3: a product unpack reads only the members it uses, and the total that
    /// leaves the decoder is bounded (headers included), so a large unused member or a
    /// gzip bomb cannot fill memory.
    #[test]
    fn unpack_stops_after_the_used_members_and_bounds_the_total() {
        let small = [7u8; 10];
        let large = [9u8; 4096];
        let archive = pack(&[("a", &small), ("b", &small), ("c", &large)]).unwrap();
        let two = unpack(&archive, 2, 3072).unwrap();
        assert_eq!(
            two,
            [("a".into(), small.to_vec()), ("b".into(), small.to_vec())]
        );
        assert!(matches!(
            unpack(&archive, 3, 3072),
            Err(BackupError::Corrupt(_))
        ));

        let half = [1u8; 1500];
        let archive = pack(&[("a", &half), ("b", &half)]).unwrap();
        assert_eq!(unpack(&archive, 2, 4096).unwrap().len(), 2);
        assert!(
            matches!(unpack(&archive, 2, 2048), Err(BackupError::Corrupt(_))),
            "each member fits, the total does not"
        );
    }

    /// [`pack`]: names longer than ustar holds get a pax `path` record and read back whole; a
    /// base64 download body decodes to the archive.
    #[test]
    fn long_names_and_base64_bodies_read_back() {
        let long = format!("./Presets/{}/{}.pb", "d".repeat(160), "n".repeat(120));
        let archive = pack(&[(&long, b"payload")]).unwrap();
        let body = STANDARD.encode(&archive).into_bytes();
        let mut seen = Vec::new();
        assert_eq!(decode_download(&archive), None);
        read_members(&decode_download(&body).unwrap()[..], |m, body| {
            let mut data = Vec::new();
            body.read_to_end(&mut data).unwrap();
            seen.push((m.path.clone(), data));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [(member_path(&long).unwrap(), b"payload".to_vec())]);
    }
}
