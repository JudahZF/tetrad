//! Blocking extension work: the store on `/media/p4/tetrad/extensions` and the GitHub
//! repositories. It runs on the extensions worker thread ([`super::extensions::LocalBackend`]),
//! never on the device loop; results come back as [`super::extensions::ExtensionInput::Job`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};
use tetrad_extensions::catalog::{Catalog, CatalogEntry, PublicKey};
use tetrad_extensions::repository::{self, Fetch, FetchError, SourceId};
use tetrad_extensions::store::{self, GeneratedIr, InstallKey, Installed, Source, Store};
use tetrad_services::http::{HttpClient, HttpError, HttpRequest, Method, Route};

/// The store root under the user partition.
pub const STORE_DIR: &str = "tetrad/extensions";
/// (O) Asset files of an extension are in `<store root>/../assets/<extension id>/`, that is
/// `/media/p4/tetrad/assets/<id>` on the unit. The user copies them there.
pub const ASSETS_DIR: &str = "assets";
/// The largest asset file: it goes to the runner in one 4 MiB frame, as JSON text.
pub const MAX_ASSET_BYTES: u64 = 3 * 1024 * 1024;
/// Asset files one scan reads.
pub const MAX_ASSETS: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionJob {
    Load,
    /// Fetch and verify the catalogs of these sources against their pinned keys.
    Refresh(Vec<Source>),
    /// Fetch a new source's published key for the trust question.
    FetchKey(SourceId),
    Install {
        source: Box<Source>,
        entry: Box<CatalogEntry>,
    },
    SaveSources(Vec<Source>),
    SetEnabled {
        key: InstallKey,
        enabled: bool,
    },
    Remove(InstallKey),
    SaveConfig {
        key: InstallKey,
        config: Value,
    },
    SaveStorage {
        key: InstallKey,
        storage: Map<String, Value>,
    },
    /// List and read the asset files of an extension's generated blocks.
    ScanAssets {
        key: InstallKey,
        /// The extension ID: the name of its asset folder.
        id: String,
        /// Each generated block with its asset file extensions (such as `.nam`).
        blocks: Vec<(String, Vec<String>)>,
    },
    /// Cache the IR an extension built for one asset.
    SaveGenerated {
        key: InstallKey,
        block: String,
        variant: String,
        ir: Box<qcdsp::Block>,
    },
    RemoveGenerated {
        key: InstallKey,
        block: String,
        variant: String,
    },
}

/// What one asset scan found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetScan {
    /// The files that read, in name order.
    pub files: Vec<AssetFile>,
    /// Every matching file as (block, variant), also the ones that did not read: a cached
    /// block of a file that is still there but did not read this time is kept.
    pub present: Vec<(String, String)>,
    /// The folder listed without an error. Without it nothing counts as removed.
    pub complete: bool,
    pub problems: Vec<String>,
}

/// One asset file for a generated block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetFile {
    pub block: String,
    /// The file stem: the variant name of the block it builds.
    pub variant: String,
    pub name: String,
    pub text: String,
}

/// One installation with everything the bridge needs to run it.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub installed: Installed,
    /// `None` for a package of DSP blocks only: it has no runner.
    pub module: Option<PathBuf>,
    pub config: Value,
    pub storage: Map<String, Value>,
    /// The IR of the package's static blocks, by block ID.
    pub blocks: BTreeMap<String, qcdsp::Block>,
    /// The cached IR the extension built from assets.
    pub generated: Vec<GeneratedIr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobResult {
    Loaded {
        sources: Vec<Source>,
        installed: Vec<Loaded>,
        catalogs: Vec<(SourceId, Catalog)>,
        problems: Vec<String>,
    },
    Refreshed(Vec<(SourceId, Result<Catalog, String>)>),
    Key(SourceId, Result<Box<PublicKey>, String>),
    Installed(Result<Box<Loaded>, String>),
    Removed(InstallKey, Result<(), String>),
    Assets {
        key: InstallKey,
        scan: AssetScan,
    },
    /// A write that answers nothing but its failure.
    Saved(Result<(), String>),
}

pub fn run<F: Fetch>(store: &Store, fetch: &mut F, job: ExtensionJob) -> JobResult {
    let text = |error: &dyn std::fmt::Display| error.to_string();
    match job {
        ExtensionJob::Load => {
            let mut state = store.load();
            let mut problems = Vec::new();
            let installed = (state.installed.iter())
                .map(|installed| {
                    let (blocks, generated) = block_irs(store, installed, &mut problems);
                    Loaded {
                        module: store.module_path(installed),
                        config: (state.configs.remove(&installed.key))
                            .unwrap_or_else(|| installed.current.manifest.default_config()),
                        storage: state.kv.remove(&installed.key).unwrap_or_default(),
                        installed: installed.clone(),
                        blocks,
                        generated,
                    }
                })
                .collect();
            state.problems.extend(problems);
            JobResult::Loaded {
                sources: state.sources,
                installed,
                catalogs: state.catalogs.into_iter().collect(),
                problems: state.problems,
            }
        }
        ExtensionJob::Refresh(sources) => JobResult::Refreshed(
            (sources.into_iter())
                .map(|source| {
                    let refreshed = repository::refresh(fetch, &source.source, &source.key)
                        .map_err(|e| text(&e))
                        .and_then(|refreshed| {
                            store
                                .save_catalog(&source.source, &refreshed.json, &refreshed.signature)
                                .map_err(|e| text(&e))?;
                            Ok(refreshed.catalog)
                        });
                    (source.source, refreshed)
                })
                .collect(),
        ),
        ExtensionJob::FetchKey(source) => {
            let key = repository::fetch_key(fetch, &source)
                .map(Box::new)
                .map_err(|e| text(&e));
            JobResult::Key(source, key)
        }
        ExtensionJob::Install { source, entry } => JobResult::Installed(
            repository::download(fetch, &source.source, &entry)
                .map_err(|e| text(&e))
                .and_then(|bytes| {
                    (store.install(&source.source, &source.key, &entry, &bytes))
                        .map_err(|e| text(&e))
                })
                .and_then(|installed| loaded(store, installed).map(Box::new)),
        ),
        ExtensionJob::SaveSources(sources) => {
            JobResult::Saved(store.save_sources(&sources).map_err(|e| text(&e)))
        }
        ExtensionJob::SetEnabled { key, enabled } => JobResult::Saved(
            store
                .set_enabled(&key, enabled)
                .map(drop)
                .map_err(|e| text(&e)),
        ),
        ExtensionJob::Remove(key) => {
            let removed = store.remove(&key).map_err(|e| text(&e));
            JobResult::Removed(key, removed)
        }
        ExtensionJob::SaveConfig { key, config } => {
            JobResult::Saved(store.save_config(&key, &config).map_err(|e| text(&e)))
        }
        ExtensionJob::SaveStorage { key, storage } => {
            JobResult::Saved(store.save_kv(&key, &storage).map_err(|e| text(&e)))
        }
        ExtensionJob::ScanAssets { key, id, blocks } => {
            let dir = assets_dir(store.root(), &id);
            JobResult::Assets {
                key,
                scan: scan_assets(&dir, &blocks),
            }
        }
        ExtensionJob::SaveGenerated {
            key,
            block,
            variant,
            ir,
        } => JobResult::Saved(
            (store.save_generated(&key, &block, &variant, &ir)).map_err(|e| text(&e)),
        ),
        ExtensionJob::RemoveGenerated {
            key,
            block,
            variant,
        } => {
            JobResult::Saved((store.remove_generated(&key, &block, &variant)).map_err(|e| text(&e)))
        }
    }
}

/// The static block IR of the package and the cached generated IR. A package that no longer
/// reads keeps its runner; its blocks are left out with a problem line.
fn block_irs(
    store: &Store,
    installed: &Installed,
    problems: &mut Vec<String>,
) -> (BTreeMap<String, qcdsp::Block>, Vec<GeneratedIr>) {
    let key = &installed.key;
    let manifest = &installed.current.manifest;
    let blocks = match manifest.blocks.iter().any(|block| !block.generated) {
        false => BTreeMap::new(),
        true => store.blocks(installed).unwrap_or_else(|error| {
            problems.push(format!("{key}: DSP blocks: {error}"));
            BTreeMap::new()
        }),
    };
    let generated = match manifest.blocks.iter().any(|block| block.generated) {
        false => Vec::new(),
        true => store.generated(key).unwrap_or_else(|error| {
            problems.push(format!("{key}: built DSP blocks: {error}"));
            Vec::new()
        }),
    };
    (blocks, generated)
}

/// `<store root>/../assets/<id>`.
pub fn assets_dir(store_root: &Path, id: &str) -> PathBuf {
    store_root
        .parent()
        .unwrap_or(store_root)
        .join(ASSETS_DIR)
        .join(id)
}

/// Reads every asset file of `blocks` in `dir`, sorted by name. The folder is made when it is
/// missing, so the user sees where to copy files. A file with another extension is skipped
/// quietly; a file that is too large, not UTF-8 or badly named gives a problem line.
fn scan_assets(dir: &Path, blocks: &[(String, Vec<String>)]) -> AssetScan {
    let mut scan = AssetScan::default();
    let listed = std::fs::create_dir_all(dir).and_then(|()| std::fs::read_dir(dir));
    let mut paths: Vec<PathBuf> = match listed {
        Ok(entries) => entries.flatten().map(|entry| entry.path()).collect(),
        Err(error) => {
            scan.problems.push(format!("{}: {error}", dir.display()));
            return scan;
        }
    };
    scan.complete = true;
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        let Some((block, extension)) = blocks.iter().find_map(|(block, extensions)| {
            let extension = extensions.iter().find(|e| lower.ends_with(e.as_str()))?;
            Some((block, extension))
        }) else {
            continue;
        };
        let variant = &name[..name.len() - extension.len()];
        if !store::variant_ok(variant) {
            scan.problems.push(format!(
                "{name}: use only letters, digits, spaces, '.', '_' and '-' (48 at most)"
            ));
            continue;
        }
        scan.present.push((block.clone(), variant.to_owned()));
        if scan.files.len() == MAX_ASSETS {
            scan.problems.push(format!(
                "more than {MAX_ASSETS} asset files; {name} is left out"
            ));
            continue;
        }
        match read_asset(&path) {
            Ok(text) => scan.files.push(AssetFile {
                block: block.clone(),
                variant: variant.to_owned(),
                name: name.to_owned(),
                text,
            }),
            Err(problem) => scan.problems.push(format!("{name}: {problem}")),
        }
    }
    scan
}

/// A regular file of at most [`MAX_ASSET_BYTES`] of UTF-8 text. The read is bounded, so a file
/// that grows (or a FIFO) cannot make it read without end.
fn read_asset(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    if !file.metadata().is_ok_and(|meta| meta.is_file()) {
        return Err("not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_ASSET_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_ASSET_BYTES {
        return Err(format!("larger than {} MiB", MAX_ASSET_BYTES >> 20));
    }
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text".into())
}

fn loaded(store: &Store, installed: Installed) -> Result<Loaded, String> {
    let manifest = &installed.current.manifest;
    let mut problems = Vec::new();
    let (blocks, generated) = block_irs(store, &installed, &mut problems);
    if let Some(problem) = problems.into_iter().next() {
        return Err(problem);
    }
    Ok(Loaded {
        module: store.module_path(&installed),
        blocks,
        generated,
        // An update can make the saved settings invalid; the package is installed already, so
        // the defaults take over, as when the store loads.
        config: store
            .config(&installed.key, manifest)
            .unwrap_or_else(|_| manifest.default_config()),
        storage: store.kv(&installed.key).map_err(|e| e.to_string())?,
        installed,
    })
}

/// [`Fetch`] over the device's direct HTTP client: GET, HTTPS redirects followed.
pub struct HttpFetch<H>(pub H);

impl<H: HttpClient> Fetch for HttpFetch<H> {
    fn get(&mut self, url: &str, max_bytes: usize) -> Result<Vec<u8>, FetchError> {
        let request = HttpRequest::new(Method::Get, url, Route::Product, Duration::from_secs(60))
            .header("User-Agent", "Tetrad")
            .follow_redirects();
        let response = self.0.send(&request).map_err(|error| match error {
            HttpError::Timeout => FetchError::Failed("timed out".into()),
            error => FetchError::Failed(error.to_string()),
        })?;
        match response.status {
            404 => Err(FetchError::NotFound),
            status if !(200..300).contains(&status) => {
                Err(FetchError::Failed(format!("HTTP {status}")))
            }
            _ if response.body.len() > max_bytes => Err(FetchError::TooLarge(max_bytes)),
            _ => Ok(response.body),
        }
    }
}
