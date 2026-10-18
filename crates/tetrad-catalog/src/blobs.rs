use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use qc_format::blob::{CodeRelocation, ModuleBlob, Section, validate_blob_name};
use serde::Serialize;

use crate::{Catalog, CatalogError, Result};

const MAX_BLOB_BYTES: u64 = 32 * 1024 * 1024;
const MAX_BLOB_FILES: usize = 16_384;
const MAX_BATCH_BYTES: u64 = 512 * 1024 * 1024;

/// Run qc-format's semantic validation and bound Code/Isa relocation source
/// offsets by section contents, in 2-byte Code or 6-byte Isa address units.
/// Relocation kind, target, and instruction patch width remain unresolved.
pub fn validate_blob(blob: &ModuleBlob) -> qc_format::Result<()> {
    blob.validate()?;
    for group in &blob.relocations {
        if !matches!(group.section, Section::Code | Section::Isa) {
            continue;
        }
        let bytes = blob.content(group.section).expect("validated section");
        let units = bytes.len() / group.section.unit_bytes() as usize;
        // Stock validation has already checked the record width. Read source
        // offsets directly to avoid allocating another decoded record vector.
        for record in group.words.chunks_exact(CodeRelocation::WORDS) {
            let offset = record[1];
            if u64::from(offset) >= units as u64 {
                return Err(qc_format::FormatError::InvalidBlob(format!(
                    "{:?} relocation offset {offset} is outside {units} address units ({} bytes)",
                    group.section,
                    bytes.len()
                )));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobStatus {
    Parsed,
    Missing,
    Malformed,
    Unsafe,
    ReadError,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlobInspection {
    pub blob_name: String,
    pub filename: Option<String>,
    pub model_ids: Vec<u32>,
    pub module_ids: Vec<u32>,
    pub referenced: bool,
    pub status: BlobStatus,
    pub byte_len: Option<u64>,
    /// None means structure parsing was not successful or not attempted.
    pub roundtrip_ok: Option<bool>,
    /// Stock validation plus Code/Isa source bounds, independent of parsing.
    pub semantic_valid: Option<bool>,
    pub semantic_error: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BlobReport {
    pub entries: Vec<BlobInspection>,
    pub files_found: usize,
    pub referenced_names: usize,
    pub unreferenced_files: usize,
    pub non_blob_models: usize,
    pub parsed: usize,
    pub roundtrip_ok: usize,
    pub semantic_valid: usize,
    pub missing_referenced: usize,
    pub malformed: usize,
    pub unsafe_paths: usize,
    pub read_errors: usize,
    pub roundtrip_failures: usize,
    pub semantic_failures: usize,
}

impl BlobReport {
    /// Optional plugin/non-blob entries and unreferenced valid files are not
    /// errors. Semantic failures remain explicit errors; no stock exception is
    /// silently treated as valid.
    pub fn has_errors(&self) -> bool {
        self.entries.iter().any(|entry| {
            matches!(
                entry.status,
                BlobStatus::Malformed | BlobStatus::Unsafe | BlobStatus::ReadError
            ) || (entry.status == BlobStatus::Missing && entry.referenced)
                || entry.roundtrip_ok == Some(false)
                || entry.semantic_valid == Some(false)
        })
    }
}

pub(crate) fn inspect(catalog: &Catalog, root: &Path) -> Result<BlobReport> {
    let directory = Directory::open(root)?;
    let mut references = BTreeMap::<String, (Vec<u32>, Vec<u32>)>::new();
    let mut report = BlobReport::default();
    for model in catalog.models.values() {
        if let Some(name) = model.blob.as_ref().filter(|s| !s.is_empty()) {
            references.entry(name.clone()).or_default().0.push(model.id);
        } else {
            report.non_blob_models += 1;
        }
    }
    for module in catalog.modules.values() {
        if let Some(name) = module.blob_name.as_ref().filter(|s| !s.is_empty()) {
            references
                .entry(name.clone())
                .or_default()
                .1
                .push(module.id);
        }
    }
    if references.len() > MAX_BLOB_FILES {
        return Err(CatalogError::Limit("blob reference count"));
    }
    report.referenced_names = references.len();
    let files = directory.raw_names()?;
    report.files_found = files.len();
    let names: BTreeSet<_> = references.keys().chain(files.iter()).cloned().collect();
    if names.len() > MAX_BLOB_FILES {
        return Err(CatalogError::Limit("blob batch entries"));
    }
    let mut batch_bytes = 0;
    for name in names {
        let (model_ids, module_ids) = references.remove(&name).unwrap_or_default();
        let referenced = !model_ids.is_empty() || !module_ids.is_empty();
        if !referenced {
            report.unreferenced_files += 1;
        }
        let mut entry = BlobInspection {
            blob_name: name.clone(),
            filename: None,
            model_ids,
            module_ids,
            referenced,
            status: BlobStatus::Unsafe,
            byte_len: None,
            roundtrip_ok: None,
            semantic_valid: None,
            semantic_error: None,
            error: None,
        };
        if validate_blob_name(&name).is_err() {
            entry.error =
                Some("blob name must contain 1–80 ASCII letters, digits or underscores".into());
        } else {
            let filename = format!("{name}.raw");
            entry.filename = Some(filename.clone());
            match directory.read(&filename) {
                Ok(bytes) => {
                    batch_bytes += bytes.len() as u64;
                    if batch_bytes > MAX_BATCH_BYTES {
                        return Err(CatalogError::Limit("blob batch bytes"));
                    }
                    entry.byte_len = Some(bytes.len() as u64);
                    match ModuleBlob::parse(&bytes) {
                        Ok(blob) => {
                            entry.status = BlobStatus::Parsed;
                            match blob.to_bytes() {
                                Ok(encoded) => entry.roundtrip_ok = Some(encoded == bytes),
                                Err(error) => {
                                    entry.roundtrip_ok = Some(false);
                                    entry.error = Some(error.to_string());
                                }
                            }
                            match validate_blob(&blob) {
                                Ok(()) => entry.semantic_valid = Some(true),
                                Err(error) => {
                                    entry.semantic_valid = Some(false);
                                    entry.semantic_error = Some(error.to_string());
                                }
                            }
                        }
                        Err(error) => {
                            entry.status = BlobStatus::Malformed;
                            entry.error = Some(error.to_string());
                        }
                    }
                }
                Err((status, error)) => {
                    entry.status = status;
                    entry.error = Some(error);
                }
            }
        }
        match entry.status {
            BlobStatus::Parsed => report.parsed += 1,
            BlobStatus::Missing => report.missing_referenced += usize::from(entry.referenced),
            BlobStatus::Malformed => report.malformed += 1,
            BlobStatus::Unsafe => report.unsafe_paths += 1,
            BlobStatus::ReadError => report.read_errors += 1,
        }
        report.roundtrip_ok += usize::from(entry.roundtrip_ok == Some(true));
        report.roundtrip_failures += usize::from(entry.roundtrip_ok == Some(false));
        report.semantic_valid += usize::from(entry.semantic_valid == Some(true));
        report.semantic_failures += usize::from(entry.semantic_valid == Some(false));
        report.entries.push(entry);
    }
    Ok(report)
}

/// Linux's descriptor path anchors all operations to the opened directory,
/// even if that directory is renamed during the batch. O_NOFOLLOW rejects the
/// final filename atomically. O_PATH obtains metadata without opening a
/// FIFO/device for reading. A held regular-file descriptor is then reopened
/// through procfs. This fails closed when procfs is not available.
#[cfg(target_os = "linux")]
struct Directory {
    _file: File,
    anchor: std::path::PathBuf,
}

#[cfg(target_os = "linux")]
impl Directory {
    fn open(root: &Path) -> Result<Self> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(root)?;
        let anchor = std::path::PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        Ok(Self {
            _file: file,
            anchor,
        })
    }

    fn raw_names(&self) -> Result<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        for (index, entry) in std::fs::read_dir(&self.anchor)?.enumerate() {
            if index >= MAX_BLOB_FILES {
                return Err(CatalogError::Limit("directory entry count"));
            }
            let entry = entry?;
            let filename = entry.file_name();
            // No unrelated entry is opened or inspected. Non-UTF8 raw names
            // cannot be represented in XML and cause an explicit batch error.
            if Path::new(&filename)
                .extension()
                .is_some_and(|ext| ext == "raw")
            {
                let filename = filename
                    .to_str()
                    .ok_or_else(|| CatalogError::Schema("non-UTF8 .raw filename".into()))?;
                if let Some(name) = filename.strip_suffix(".raw") {
                    names.insert(name.to_owned());
                }
            }
        }
        Ok(names)
    }

    fn read(&self, filename: &str) -> std::result::Result<Vec<u8>, (BlobStatus, String)> {
        use std::os::unix::fs::OpenOptionsExt;
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_PATH)
            .open(self.anchor.join(filename))
            .map_err(|error| {
                let status = if error.kind() == std::io::ErrorKind::NotFound {
                    BlobStatus::Missing
                } else if error.raw_os_error() == Some(libc::ELOOP) {
                    // Linux ELOOP: refused symlink.
                    BlobStatus::Unsafe
                } else {
                    BlobStatus::ReadError
                };
                (status, error.to_string())
            })?;
        let metadata = handle
            .metadata()
            .map_err(|error| (BlobStatus::ReadError, error.to_string()))?;
        if !metadata.is_file() {
            return Err((BlobStatus::Unsafe, "blob is not a regular file".into()));
        }
        if metadata.len() > MAX_BLOB_BYTES {
            return Err((BlobStatus::ReadError, "blob exceeds byte limit".into()));
        }
        use std::os::fd::AsRawFd;
        let file = File::open(format!("/proc/self/fd/{}", handle.as_raw_fd()))
            .map_err(|error| (BlobStatus::ReadError, error.to_string()))?;
        let mut bytes = Vec::new();
        file.take(MAX_BLOB_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| (BlobStatus::ReadError, error.to_string()))?;
        if bytes.len() as u64 > MAX_BLOB_BYTES {
            return Err((BlobStatus::ReadError, "blob exceeds byte limit".into()));
        }
        Ok(bytes)
    }
}

#[cfg(not(target_os = "linux"))]
struct Directory;

#[cfg(not(target_os = "linux"))]
impl Directory {
    fn open(_root: &Path) -> Result<Self> {
        Err(CatalogError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "secure blob inspection requires Linux procfs",
        )))
    }
    fn raw_names(&self) -> Result<BTreeSet<String>> {
        unreachable!("open fails on unsupported platforms")
    }
    fn read(&self, _filename: &str) -> std::result::Result<Vec<u8>, (BlobStatus, String)> {
        unreachable!("open fails on unsupported platforms")
    }
}
