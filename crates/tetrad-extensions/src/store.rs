//! The on-device extension store. Every write is atomic: a temporary file in the same
//! directory, fsync, rename, fsync of the directory.
//!
//! Layout under the root:
//!
//! ```text
//! sources.json                                 [{ source, key, default, enabled }]
//! catalogs/<source>.json, <source>.sig         verified catalog bytes, re-verified on load
//! packages/<source>/<id>/<version>.tetradext   immutable package
//! packages/<source>/<id>/<version>.mjs         its module, the file the runner starts
//!                                              (a blocks-only package has none)
//! installed/<source>~<id>.json                 installation record
//! config/<source>~<id>.json                    settings object
//! kv/<source>~<id>.json                        extension storage object
//! blocks/<source>~<id>/<block>/<variant>.json  generated block IR (a cache)
//! ```
//!
//! `<source>` is [`SourceId::dir_name`]. A removed installation keeps its config and storage;
//! its generated IR is deleted.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::catalog::{Catalog, CatalogEntry, PublicKey, sha256_hex};
use crate::manifest::{self, Manifest};
use crate::package::{Package, PackageError};
use crate::repository::{self, AssetMismatch, DEFAULT_KEY, DEFAULT_SOURCE, SourceId};

/// Largest storage object of one installation, serialized.
pub const MAX_KV_BYTES: usize = 256 * 1024;
pub const MAX_KV_KEY: usize = 128;
/// Largest config file; a config is at most 16 small settings.
const MAX_CONFIG_BYTES: usize = 16 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024;
/// Largest generated IR file. The IR arrives in one protocol frame.
const MAX_IR_BYTES: usize = crate::protocol::MAX_FRAME;
/// Longest variant name of generated IR.
pub const MAX_VARIANT: usize = 48;
const TEMP_SUFFIX: &str = ".tmp";

/// One installation: an extension ID from one source.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstallKey {
    pub source: SourceId,
    pub id: String,
}

impl InstallKey {
    /// `<source dir>~<id>`; neither part can contain `~`.
    pub fn file_stem(&self) -> String {
        format!("{}~{}", self.source.dir_name(), self.id)
    }
}

impl fmt::Display for InstallKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source, self.id)
    }
}

impl FromStr for InstallKey {
    type Err = StoreError;

    /// Parses `owner/repo:id`.
    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        let bad = || StoreError::BadKey(text.to_owned());
        let (source, id) = text.split_once(':').ok_or_else(bad)?;
        manifest::validate_id(id).map_err(|_| bad())?;
        Ok(Self {
            source: source.parse().map_err(|_| bad())?,
            id: id.to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub source: SourceId,
    /// The pinned signing key.
    pub key: PublicKey,
    /// The bundled default repository.
    pub default: bool,
    pub enabled: bool,
}

impl Source {
    pub fn default_source() -> Self {
        Self {
            source: DEFAULT_SOURCE.parse().expect("DEFAULT_SOURCE parses"),
            key: PublicKey::from_base64(DEFAULT_KEY).expect("DEFAULT_KEY parses"),
            default: true,
            enabled: true,
        }
    }
}

/// One installed version of an extension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub version: Version,
    /// SHA-256 of the package bytes.
    pub sha256: String,
    /// Fingerprint of the key that signed the catalog it came from.
    pub signer: String,
    pub manifest: Manifest,
    /// The package has a module to run. A data-only package has none.
    #[serde(default = "records_before_blocks_have_a_module")]
    pub has_module: bool,
}

fn records_before_blocks_have_a_module() -> bool {
    true
}

/// One cached result of a generated block.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedIr {
    pub block: String,
    pub variant: String,
    pub ir: qcdsp::Block,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Installed {
    pub key: InstallKey,
    pub enabled: bool,
    pub current: Release,
    /// The version before the last update, kept on disk for [`Store::rollback`].
    pub previous: Option<Release>,
}

/// Everything the store holds, as read by [`Store::load`].
#[derive(Debug, Default)]
pub struct StoreState {
    pub sources: Vec<Source>,
    pub installed: Vec<Installed>,
    pub catalogs: HashMap<SourceId, Catalog>,
    /// Validated config of each installation, defaults filled in.
    pub configs: HashMap<InstallKey, Value>,
    pub kv: HashMap<InstallKey, Map<String, Value>>,
    /// Files that were skipped or repaired, for the log. Loading never fails on bad files.
    pub problems: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {error}")]
    Io { path: PathBuf, error: io::Error },
    #[error("{path}: {reason}")]
    Corrupt { path: PathBuf, reason: String },
    #[error("{0:?} is not an installation key (owner/repo:id)")]
    BadKey(String),
    #[error("package: {0}")]
    Package(#[from] PackageError),
    #[error("package does not match the signed catalog: {0}")]
    Mismatch(&'static str),
    #[error("{0} already exists with different contents")]
    Conflict(PathBuf),
    #[error("{0} is not installed")]
    NotInstalled(InstallKey),
    #[error("{0} has no previous version")]
    NoPrevious(InstallKey),
    #[error("{installed} is installed; {offered} is older (use rollback to go back)")]
    Downgrade {
        installed: Version,
        offered: Version,
    },
    #[error("storage is over its limit: {0}")]
    Limit(&'static str),
    #[error(
        "{0:?} is not a block ID ([a-z0-9-]{{1,32}}) or a variant name ([A-Za-z0-9 ._-]{{1,48}}, not starting with '.')"
    )]
    BadName(String),
    #[error("block IR: {0}")]
    Ir(#[from] qcdsp::Error),
}

type Result<T> = std::result::Result<T, StoreError>;

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |error| StoreError::Io {
        path: path.to_owned(),
        error,
    }
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Creates the root if needed. Nothing else is read until [`Store::load`].
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        create_dirs(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn sources_path(&self) -> PathBuf {
        self.root.join("sources.json")
    }

    fn catalog_paths(&self, source: &SourceId) -> (PathBuf, PathBuf) {
        let dir = self.root.join("catalogs");
        let name = source.dir_name();
        (
            dir.join(format!("{name}.json")),
            dir.join(format!("{name}.sig")),
        )
    }

    fn package_dir(&self, key: &InstallKey) -> PathBuf {
        self.root
            .join("packages")
            .join(key.source.dir_name())
            .join(&key.id)
    }

    fn package_path(&self, key: &InstallKey, version: &Version) -> PathBuf {
        self.package_dir(key).join(format!("{version}.tetradext"))
    }

    fn module_file(&self, key: &InstallKey, version: &Version) -> PathBuf {
        self.package_dir(key).join(format!("{version}.mjs"))
    }

    fn record_path(&self, key: &InstallKey) -> PathBuf {
        self.root
            .join("installed")
            .join(format!("{}.json", key.file_stem()))
    }

    fn config_path(&self, key: &InstallKey) -> PathBuf {
        self.root
            .join("config")
            .join(format!("{}.json", key.file_stem()))
    }

    fn kv_path(&self, key: &InstallKey) -> PathBuf {
        self.root
            .join("kv")
            .join(format!("{}.json", key.file_stem()))
    }

    fn generated_dir(&self, key: &InstallKey) -> PathBuf {
        self.root.join("blocks").join(key.file_stem())
    }

    /// The module file to start the runner with; `None` for a data-only package (themes and
    /// static DSP blocks). A theme-only package installed before `has_module` left an empty
    /// module file, which also counts as none.
    pub fn module_path(&self, installed: &Installed) -> Option<PathBuf> {
        let release = &installed.current;
        let path = self.module_file(&installed.key, &release.version);
        let empty = fs::metadata(&path).is_ok_and(|meta| meta.len() == 0);
        (release.has_module && !empty).then_some(path)
    }

    /// Reads everything, re-verifying cached catalogs against pinned keys and installed
    /// packages against their recorded SHA-256. Bad files are skipped and reported in
    /// [`StoreState::problems`]; stale temporary files are deleted.
    pub fn load(&self) -> StoreState {
        let mut state = StoreState::default();
        remove_temp_files(&self.root, 3, &mut state.problems);

        state.sources = match read_bounded(&self.sources_path(), 64 * 1024) {
            Ok(None) => vec![Source::default_source()],
            Ok(Some(bytes)) => match serde_json::from_slice(&bytes) {
                Ok(sources) => sources,
                Err(e) => {
                    state
                        .problems
                        .push(format!("sources.json: {e}; using the default source"));
                    vec![Source::default_source()]
                }
            },
            Err(e) => {
                state
                    .problems
                    .push(format!("{e}; using the default source"));
                vec![Source::default_source()]
            }
        };

        for source in &state.sources {
            let (json_path, sig_path) = self.catalog_paths(&source.source);
            let read = read_bounded(&json_path, crate::catalog::MAX_CATALOG)
                .and_then(|json| Ok((json, read_bounded(&sig_path, 1024)?)));
            match read {
                Ok((None, _)) => {}
                Ok((Some(json), Some(sig))) => match Catalog::verify(&json, &sig, &source.key) {
                    Ok(catalog) => {
                        state.catalogs.insert(source.source.clone(), catalog);
                    }
                    Err(e) => state.problems.push(format!(
                        "{}: cached catalog skipped: {e}",
                        json_path.display()
                    )),
                },
                Ok((Some(_), None)) => state
                    .problems
                    .push(format!("{}: no signature", json_path.display())),
                Err(e) => state.problems.push(e.to_string()),
            }
        }

        for path in json_files(&self.root.join("installed"), &mut state.problems) {
            match self.load_installed(&path) {
                Ok(installed) => state.installed.push(installed),
                Err(e) => state.problems.push(format!("installation skipped: {e}")),
            }
        }

        for installed in &state.installed {
            let key = &installed.key;
            match self.config(key, &installed.current.manifest) {
                Ok(config) => {
                    state.configs.insert(key.clone(), config);
                }
                Err(e) => {
                    state.problems.push(format!("{e}; using default settings"));
                    state
                        .configs
                        .insert(key.clone(), installed.current.manifest.default_config());
                }
            }
            match self.kv(key) {
                Ok(kv) => {
                    state.kv.insert(key.clone(), kv);
                }
                Err(e) => {
                    state.problems.push(format!("{e}; storage starts empty"));
                    state.kv.insert(key.clone(), Map::new());
                }
            }
        }
        state
    }

    /// Reads a record and checks its package. A missing or changed module file is
    /// rewritten from the package.
    fn load_installed(&self, path: &Path) -> Result<Installed> {
        let bytes = read_bounded(path, MAX_RECORD_BYTES)?.ok_or_else(|| StoreError::Corrupt {
            path: path.to_owned(),
            reason: "vanished".into(),
        })?;
        let installed: Installed =
            serde_json::from_slice(&bytes).map_err(|e| StoreError::Corrupt {
                path: path.to_owned(),
                reason: e.to_string(),
            })?;
        if self.record_path(&installed.key) != path {
            return Err(StoreError::Corrupt {
                path: path.to_owned(),
                reason: "record name does not match its key".into(),
            });
        }
        self.check_release(&installed.key, &installed.current)?;
        Ok(installed)
    }

    fn check_release(&self, key: &InstallKey, release: &Release) -> Result<()> {
        match self.read_package(key, release)?.module {
            Some(module) => self.write_module(key, &release.version, &module),
            None => Ok(()),
        }
    }

    /// The static block IR of the current version, by block ID.
    pub fn blocks(&self, installed: &Installed) -> Result<BTreeMap<String, qcdsp::Block>> {
        Ok(self
            .read_package(&installed.key, &installed.current)?
            .blocks)
    }

    /// The HTML of the Remote tab of the current version, if it has one.
    pub fn remote_page(&self, installed: &Installed) -> Result<Option<String>> {
        Ok(self
            .read_package(&installed.key, &installed.current)?
            .remote_page)
    }

    /// Reads a release's package and checks it against the recorded SHA-256.
    fn read_package(&self, key: &InstallKey, release: &Release) -> Result<Package> {
        let path = self.package_path(key, &release.version);
        let bytes = read_bounded(&path, crate::package::MAX_PACKAGE)?.ok_or_else(|| {
            StoreError::Corrupt {
                path: path.clone(),
                reason: "package file is missing".into(),
            }
        })?;
        if sha256_hex(&bytes) != release.sha256 {
            return Err(StoreError::Corrupt {
                path,
                reason: "package SHA-256 does not match the record".into(),
            });
        }
        Ok(Package::decode(&bytes)?)
    }

    /// Writes the module file unless it already holds `module`.
    fn write_module(&self, key: &InstallKey, version: &Version, module: &str) -> Result<()> {
        let path = self.module_file(key, version);
        match fs::read(&path) {
            Ok(existing) if existing == module.as_bytes() => Ok(()),
            _ => write_atomic(&path, module.as_bytes()),
        }
    }

    /// Writes the source list and drops cached catalogs of sources no longer in it.
    pub fn save_sources(&self, sources: &[Source]) -> Result<()> {
        let json = serde_json::to_vec_pretty(sources).expect("sources serialize");
        write_atomic(&self.sources_path(), &json)?;
        let dir = self.root.join("catalogs");
        let keep: Vec<String> = sources.iter().map(|s| s.source.dir_name()).collect();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_error(&dir)(e)),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if !keep.iter().any(|k| k == stem) {
                remove_file(&path)?;
            }
        }
        Ok(())
    }

    /// Caches the exact verified bytes so [`Store::load`] can verify them again.
    pub fn save_catalog(&self, source: &SourceId, json: &[u8], signature: &[u8]) -> Result<()> {
        let (json_path, sig_path) = self.catalog_paths(source);
        write_atomic(&sig_path, signature)?;
        write_atomic(&json_path, json)
    }

    pub fn installed(&self, key: &InstallKey) -> Result<Option<Installed>> {
        let path = self.record_path(key);
        let Some(bytes) = read_bounded(&path, MAX_RECORD_BYTES)? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| StoreError::Corrupt {
                path,
                reason: e.to_string(),
            })
    }

    fn require(&self, key: &InstallKey) -> Result<Installed> {
        self.installed(key)?
            .ok_or_else(|| StoreError::NotInstalled(key.clone()))
    }

    fn save_record(&self, installed: &Installed) -> Result<()> {
        let json = serde_json::to_vec_pretty(installed).expect("record serializes");
        write_atomic(&self.record_path(&installed.key), &json)
    }

    /// Installs or updates to the package of `entry`, verified against that signed entry.
    /// An update keeps the replaced version as `previous`; the version before that is
    /// deleted. A fresh install is enabled; an update keeps the enabled flag.
    pub fn install(
        &self,
        source: &SourceId,
        signer: &PublicKey,
        entry: &CatalogEntry,
        package_bytes: &[u8],
    ) -> Result<Installed> {
        repository::check_asset(package_bytes, entry).map_err(|e| {
            StoreError::Mismatch(match e {
                AssetMismatch::Size => "size",
                AssetMismatch::Hash => "SHA-256",
            })
        })?;
        let package = Package::decode(package_bytes)?;
        let manifest = &package.manifest;
        if manifest.id != entry.id || manifest.version != entry.version || manifest.api != entry.api
        {
            return Err(StoreError::Mismatch("manifest ID, version or API"));
        }
        let key = InstallKey {
            source: source.clone(),
            id: entry.id.clone(),
        };
        // An older catalog (replayed, or changed between view and tap) never downgrades.
        if let Some(old) = self.installed(&key)?
            && entry.version < old.current.version
        {
            return Err(StoreError::Downgrade {
                installed: old.current.version,
                offered: entry.version.clone(),
            });
        }

        let path = self.package_path(&key, &entry.version);
        match read_bounded(&path, crate::package::MAX_PACKAGE)? {
            Some(existing) if existing != package_bytes => return Err(StoreError::Conflict(path)),
            Some(_) => {}
            None => write_atomic(&path, package_bytes)?,
        }
        if let Some(module) = &package.module {
            self.write_module(&key, &entry.version, module)?;
        }

        let release = Release {
            version: entry.version.clone(),
            sha256: entry.asset.sha256.clone(),
            signer: signer.fingerprint(),
            has_module: package.module.is_some(),
            manifest: package.manifest,
        };
        let installed = match self.installed(&key)? {
            None => Installed {
                key,
                enabled: true,
                current: release,
                previous: None,
            },
            Some(old) if old.current.version == release.version => Installed {
                current: release,
                ..old
            },
            Some(old) => {
                let dropped = old
                    .previous
                    .filter(|p| p.version != release.version)
                    .map(|p| p.version);
                let installed = Installed {
                    key,
                    enabled: old.enabled,
                    current: release,
                    previous: Some(old.current),
                };
                self.save_record(&installed)?;
                if let Some(version) = dropped {
                    self.remove_release_files(&installed.key, &version)?;
                }
                return Ok(installed);
            }
        };
        self.save_record(&installed)?;
        Ok(installed)
    }

    /// Swaps the current and previous versions.
    pub fn rollback(&self, key: &InstallKey) -> Result<Installed> {
        let old = self.require(key)?;
        let previous = old
            .previous
            .ok_or_else(|| StoreError::NoPrevious(key.clone()))?;
        self.check_release(key, &previous)?;
        let installed = Installed {
            key: old.key,
            enabled: old.enabled,
            current: previous,
            previous: Some(old.current),
        };
        self.save_record(&installed)?;
        Ok(installed)
    }

    pub fn set_enabled(&self, key: &InstallKey, enabled: bool) -> Result<Installed> {
        let installed = Installed {
            enabled,
            ..self.require(key)?
        };
        self.save_record(&installed)?;
        Ok(installed)
    }

    /// Removes the record, every package and the generated IR of the installation. Config
    /// and storage stay.
    pub fn remove(&self, key: &InstallKey) -> Result<()> {
        let record = self.record_path(key);
        if !record.exists() {
            return Err(StoreError::NotInstalled(key.clone()));
        }
        remove_file(&record)?;
        remove_dir(&self.package_dir(key))?;
        remove_dir(&self.generated_dir(key))
    }

    fn generated_path(&self, key: &InstallKey, block: &str, variant: &str) -> Result<PathBuf> {
        if !manifest::short_id_ok(block) {
            return Err(StoreError::BadName(block.to_owned()));
        }
        if !variant_ok(variant) {
            return Err(StoreError::BadName(variant.to_owned()));
        }
        Ok(self
            .generated_dir(key)
            .join(block)
            .join(format!("{variant}.json")))
    }

    /// Caches the IR an extension built for `block` from the asset named `variant`.
    pub fn save_generated(
        &self,
        key: &InstallKey,
        block: &str,
        variant: &str,
        ir: &qcdsp::Block,
    ) -> Result<()> {
        let path = self.generated_path(key, block, variant)?;
        let json = ir.to_json()?;
        if json.len() > MAX_IR_BYTES {
            return Err(StoreError::Limit("block IR is over 4 MiB"));
        }
        write_atomic(&path, json.as_bytes())
    }

    /// Every cached IR of the installation, sorted by block and variant. An entry that does
    /// not read or validate is skipped: build it again from its asset.
    pub fn generated(&self, key: &InstallKey) -> Result<Vec<GeneratedIr>> {
        let mut entries = Vec::new();
        for block_dir in sorted_entries(&self.generated_dir(key))? {
            let block = block_dir.file_name().and_then(|n| n.to_str());
            let Some(block) = block.filter(|b| manifest::short_id_ok(b)) else {
                continue;
            };
            for path in sorted_entries(&block_dir)? {
                let variant = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_suffix(".json"))
                    .filter(|v| variant_ok(v));
                let Some(variant) = variant else {
                    continue;
                };
                let ir = read_bounded(&path, MAX_IR_BYTES)
                    .ok()
                    .flatten()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .and_then(|json| qcdsp::Block::from_json(&json).ok());
                if let Some(ir) = ir {
                    entries.push(GeneratedIr {
                        block: block.to_owned(),
                        variant: variant.to_owned(),
                        ir,
                    });
                }
            }
        }
        Ok(entries)
    }

    /// Deletes one cached IR. A missing entry is not an error.
    pub fn remove_generated(&self, key: &InstallKey, block: &str, variant: &str) -> Result<()> {
        remove_file(&self.generated_path(key, block, variant)?)
    }

    fn remove_release_files(&self, key: &InstallKey, version: &Version) -> Result<()> {
        remove_file(&self.package_path(key, version))?;
        remove_file(&self.module_file(key, version))
    }

    /// The saved config checked against `manifest`, defaults filled in. No file means all
    /// defaults.
    pub fn config(&self, key: &InstallKey, manifest: &Manifest) -> Result<Value> {
        let path = self.config_path(key);
        let Some(bytes) = read_bounded(&path, MAX_CONFIG_BYTES)? else {
            return Ok(manifest.default_config());
        };
        let corrupt = |reason: String| StoreError::Corrupt {
            path: path.clone(),
            reason,
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| corrupt(e.to_string()))?;
        manifest
            .validate_config(&value)
            .map_err(|e| corrupt(e.to_string()))
    }

    /// Writes a config; validate it with [`Manifest::validate_config`] first.
    pub fn save_config(&self, key: &InstallKey, config: &Value) -> Result<()> {
        let json = serde_json::to_vec(config).expect("JSON value serializes");
        if json.len() > MAX_CONFIG_BYTES {
            return Err(StoreError::Limit("config"));
        }
        write_atomic(&self.config_path(key), &json)
    }

    pub fn kv(&self, key: &InstallKey) -> Result<Map<String, Value>> {
        let path = self.kv_path(key);
        let Some(bytes) = read_bounded(&path, MAX_KV_BYTES)? else {
            return Ok(Map::new());
        };
        let kv: Map<String, Value> =
            serde_json::from_slice(&bytes).map_err(|e| StoreError::Corrupt {
                path: path.clone(),
                reason: e.to_string(),
            })?;
        check_kv(&kv)?;
        Ok(kv)
    }

    /// Writes the whole storage object of one installation. Fails without writing when it is
    /// over [`MAX_KV_BYTES`] or a key is empty or over [`MAX_KV_KEY`] characters.
    pub fn save_kv(&self, key: &InstallKey, kv: &Map<String, Value>) -> Result<()> {
        check_kv(kv)?;
        let json = serde_json::to_vec(kv).expect("JSON value serializes");
        if json.len() > MAX_KV_BYTES {
            return Err(StoreError::Limit("storage is over 256 KiB"));
        }
        write_atomic(&self.kv_path(key), &json)
    }
}

fn check_kv(kv: &Map<String, Value>) -> Result<()> {
    if kv
        .keys()
        .any(|k| k.is_empty() || k.chars().count() > MAX_KV_KEY)
    {
        return Err(StoreError::Limit("storage keys must be 1..=128 characters"));
    }
    Ok(())
}

/// `None` when the file does not exist.
fn read_bounded(path: &Path, max: usize) -> Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(path)(e)),
    };
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut io::Read::take(file, max as u64 + 1), &mut bytes)
        .map_err(io_error(path))?;
    if bytes.len() > max {
        return Err(StoreError::Corrupt {
            path: path.to_owned(),
            reason: format!("over {max} bytes"),
        });
    }
    Ok(Some(bytes))
}

/// Creates missing directories and fsyncs the parent of each one created.
fn create_dirs(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        create_dirs(parent)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(io_error(dir)(e)),
    }
    match dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => sync_dir(parent),
        None => Ok(()),
    }
}

fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_error(dir))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().expect("store paths have a parent");
    create_dirs(dir)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .expect("store file names are UTF-8");
    let temp = dir.join(format!(".{name}{TEMP_SUFFIX}"));
    let write = || -> io::Result<()> {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    };
    write().map_err(io_error(&temp))?;
    fs::rename(&temp, path).map_err(io_error(path))?;
    sync_dir(dir)
}

fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_dir(path.parent().expect("store paths have a parent")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(path)(e)),
    }
}

fn remove_dir(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => sync_dir(dir.parent().expect("store paths have a parent")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(dir)(e)),
    }
}

/// `[A-Za-z0-9 ._-]{1,48}`, not starting with `.`: no hidden, temporary or `..` names.
pub fn variant_ok(variant: &str) -> bool {
    (1..=MAX_VARIANT).contains(&variant.len())
        && !variant.starts_with('.')
        && variant
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b'_' | b'-'))
}

/// The entries of `dir`, sorted; none when it does not exist.
fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_error(dir)(e)),
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    Ok(paths)
}

fn json_files(dir: &Path, problems: &mut Vec<String>) -> Vec<PathBuf> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            problems.push(format!("{}: {e}", dir.display()));
            return Vec::new();
        }
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
}

/// Deletes `.<name>.tmp` files left by an interrupted write, down to `depth` levels.
fn remove_temp_files(dir: &Path, depth: u32, problems: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && depth > 0 {
            remove_temp_files(&path, depth - 1, problems);
        } else if kind.is_file()
            && name.starts_with('.')
            && name.ends_with(TEMP_SUFFIX)
            && let Err(e) = remove_file(&path)
        {
            problems.push(e.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Asset;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "tetrad-store-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn source() -> SourceId {
        "Owner/repo".parse().unwrap()
    }

    fn key() -> PublicKey {
        Source::default_source().key
    }

    fn release(version: &str) -> (CatalogEntry, Vec<u8>) {
        entry_for(&Package {
            manifest: serde_json::from_value(json!({
                "id": "tetrad.test", "name": "Test", "version": version, "api": 1,
                "settings": [{"key": "on", "label": "On", "kind": "toggle", "default": true}]
            }))
            .unwrap(),
            module: Some(format!(
                "export default {{ activate() {{}} }}; // {version}"
            )),
            blocks: BTreeMap::new(),
            remote_page: None,
        })
    }

    fn entry_for(package: &Package) -> (CatalogEntry, Vec<u8>) {
        let bytes = package.encode().unwrap();
        let m = &package.manifest;
        let entry = CatalogEntry {
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            description: String::new(),
            api: m.api,
            asset: Asset {
                tag: "t".into(),
                name: package.file_name(),
                size: bytes.len() as u64,
                sha256: sha256_hex(&bytes),
            },
        };
        (entry, bytes)
    }

    #[test]
    fn empty_store_has_the_default_source() {
        let dir = TempDir::new();
        let state = Store::open(&dir.0).unwrap().load();
        assert_eq!(state.sources, [Source::default_source()]);
        assert!(state.installed.is_empty() && state.problems.is_empty());
    }

    #[test]
    fn install_update_rollback_remove() {
        let dir = TempDir::new();
        let store = Store::open(&dir.0).unwrap();
        let (v1, b1) = release("1.0.0");
        let (v2, b2) = release("1.1.0");
        let (v3, b3) = release("1.2.0");

        let installed = store.install(&source(), &key(), &v1, &b1).unwrap();
        assert!(installed.enabled && installed.previous.is_none());
        let module = fs::read_to_string(store.module_path(&installed).unwrap()).unwrap();
        assert!(module.ends_with("// 1.0.0"));

        store.set_enabled(&installed.key, false).unwrap();
        store.install(&source(), &key(), &v2, &b2).unwrap();
        let installed = store.install(&source(), &key(), &v3, &b3).unwrap();
        assert!(!installed.enabled, "an update keeps the enabled flag");
        assert_eq!(installed.previous.as_ref().unwrap().version, v2.version);
        assert!(
            !store.package_path(&installed.key, &v1.version).exists(),
            "the version before previous is deleted"
        );

        let rolled = store.rollback(&installed.key).unwrap();
        assert_eq!(rolled.current.version, v2.version);
        assert_eq!(rolled.previous.unwrap().version, v3.version);
        let older = store.install(&source(), &key(), &v1, &b1);
        assert!(
            matches!(older, Err(StoreError::Downgrade { .. })),
            "only rollback goes back"
        );

        let key = installed.key.clone();
        store.save_config(&key, &json!({"on": false})).unwrap();
        let state = store.load();
        assert!(state.problems.is_empty(), "{:?}", state.problems);
        assert_eq!(state.installed.len(), 1);
        assert_eq!(state.configs[&key], json!({"on": false}));

        store.remove(&key).unwrap();
        assert!(store.installed(&key).unwrap().is_none());
        assert!(!store.package_dir(&key).exists());
        assert!(store.config_path(&key).exists(), "config outlives removal");
    }

    #[test]
    fn install_refuses_mismatches_and_never_overwrites() {
        let dir = TempDir::new();
        let store = Store::open(&dir.0).unwrap();
        let (entry, bytes) = release("1.0.0");
        let mut tampered = bytes.clone();
        *tampered.last_mut().unwrap() = b' ';
        assert!(matches!(
            store.install(&source(), &key(), &entry, &tampered),
            Err(StoreError::Mismatch(_))
        ));

        // A signed entry whose package claims another version.
        let (other, other_bytes) = release("2.0.0");
        let lying = CatalogEntry {
            asset: other.asset.clone(),
            ..entry.clone()
        };
        assert!(matches!(
            store.install(&source(), &key(), &lying, &other_bytes),
            Err(StoreError::Mismatch(_))
        ));

        // A different file already at the immutable path.
        let install_key = InstallKey {
            source: source(),
            id: entry.id.clone(),
        };
        write_atomic(&store.package_path(&install_key, &entry.version), b"other").unwrap();
        assert!(matches!(
            store.install(&source(), &key(), &entry, &bytes),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn load_repairs_and_skips_bad_files() {
        let dir = TempDir::new();
        let store = Store::open(&dir.0).unwrap();
        let (entry, bytes) = release("1.0.0");
        let installed = store.install(&source(), &key(), &entry, &bytes).unwrap();

        // A lost module file is rewritten; a stale temp file is deleted.
        fs::remove_file(store.module_path(&installed).unwrap()).unwrap();
        let temp = dir.0.join("installed").join(".x.json.tmp");
        fs::write(&temp, b"partial").unwrap();
        // A corrupt kv file starts empty; an unverifiable catalog is skipped.
        fs::create_dir_all(dir.0.join("kv")).unwrap();
        fs::write(store.kv_path(&installed.key), b"{").unwrap();
        store
            .save_catalog(&Source::default_source().source, b"{}", b"AAAA")
            .unwrap();

        let state = store.load();
        assert_eq!(state.installed.len(), 1);
        assert!(store.module_path(&installed).unwrap().exists());
        assert!(!temp.exists());
        assert!(state.kv[&installed.key].is_empty());
        assert!(state.catalogs.is_empty());
        assert_eq!(state.problems.len(), 2, "{:?}", state.problems);

        // A changed package fails its recorded SHA-256 and is not loaded.
        fs::write(store.package_path(&installed.key, &entry.version), b"x").unwrap();
        assert!(store.load().installed.is_empty());
    }

    #[test]
    fn blocks_only_install_and_generated_cache() {
        let dir = TempDir::new();
        let store = Store::open(&dir.0).unwrap();
        let (entry, bytes) = entry_for(&Package {
            manifest: serde_json::from_value(json!({
                "id": "tetrad.blocks", "name": "Blocks", "version": "1.0.0", "api": 2,
                "blocks": [{"id": "gain", "name": "Gain", "category": "utility"}]
            }))
            .unwrap(),
            module: None,
            blocks: BTreeMap::from([("gain".into(), qcdsp::examples::gain().unwrap())]),
            remote_page: None,
        });
        let installed = store.install(&source(), &key(), &entry, &bytes).unwrap();
        assert_eq!(store.module_path(&installed), None, "nothing to run");
        assert_eq!(
            store.blocks(&installed).unwrap().keys().collect::<Vec<_>>(),
            ["gain"]
        );
        let state = store.load();
        assert!(state.problems.is_empty(), "{:?}", state.problems);
        assert_eq!(state.installed, std::slice::from_ref(&installed));

        let key = &installed.key;
        let ir = qcdsp::examples::saturator().unwrap();
        store
            .save_generated(key, "nam", "Plexi Crunch_2.1", &ir)
            .unwrap();
        store.save_generated(key, "nam", "Clean", &ir).unwrap();
        for (block, variant) in [
            ("nam", "../x"),
            ("nam", ".hidden"),
            ("nam", ""),
            ("../x", "a"),
        ] {
            assert!(
                matches!(
                    store.save_generated(key, block, variant, &ir),
                    Err(StoreError::BadName(_))
                ),
                "{block}/{variant}"
            );
        }
        // An entry that no longer validates is skipped, not fatal.
        fs::write(store.generated_dir(key).join("nam/Broken.json"), b"{").unwrap();
        let names = |store: &Store| -> Vec<String> {
            let generated = store.generated(key).unwrap();
            generated
                .iter()
                .map(|g| format!("{}/{}", g.block, g.variant))
                .collect()
        };
        assert_eq!(names(&store), ["nam/Clean", "nam/Plexi Crunch_2.1"]);
        assert_eq!(
            store.generated(key).unwrap()[0].ir.channels(),
            qcdsp::Channels::Mono
        );

        store.remove_generated(key, "nam", "Clean").unwrap();
        assert_eq!(names(&store), ["nam/Plexi Crunch_2.1"]);
        store.remove(key).unwrap();
        assert!(
            !store.generated_dir(key).exists(),
            "remove deletes generated IR"
        );
    }

    #[test]
    fn kv_bounds() {
        let dir = TempDir::new();
        let store = Store::open(&dir.0).unwrap();
        let key: InstallKey = "owner/repo:tetrad.test".parse().unwrap();
        assert_eq!(key.to_string(), "owner/repo:tetrad.test");
        let mut kv = Map::new();
        kv.insert("k".repeat(MAX_KV_KEY + 1), json!(1));
        assert!(matches!(
            store.save_kv(&key, &kv),
            Err(StoreError::Limit(_))
        ));
        let mut kv = Map::new();
        kv.insert("big".into(), json!("x".repeat(MAX_KV_BYTES)));
        assert!(matches!(
            store.save_kv(&key, &kv),
            Err(StoreError::Limit(_))
        ));
        let mut kv = Map::new();
        kv.insert("mappings".into(), json!([1, 2]));
        store.save_kv(&key, &kv).unwrap();
        assert_eq!(store.kv(&key).unwrap(), kv);
    }
}
