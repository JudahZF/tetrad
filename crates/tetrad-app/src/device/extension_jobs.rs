//! Blocking extension work: the store on `/media/p4/tetrad/extensions` and the GitHub
//! repositories. It runs on the extensions worker thread ([`super::extensions::LocalBackend`]),
//! never on the device loop; results come back as [`super::extensions::ExtensionInput::Job`].

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value};
use tetrad_extensions::catalog::{Catalog, CatalogEntry, PublicKey};
use tetrad_extensions::repository::{self, Fetch, FetchError, SourceId};
use tetrad_extensions::store::{InstallKey, Installed, Source, Store};
use tetrad_services::http::{HttpClient, HttpError, HttpRequest, Method, Route};

/// The store root under the user partition.
pub const STORE_DIR: &str = "tetrad/extensions";

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
}

/// One installation with everything the bridge needs to run it.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub installed: Installed,
    pub module: PathBuf,
    /// The package has code (`Store::has_module`); a theme-only package has none.
    pub has_module: bool,
    pub config: Value,
    pub storage: Map<String, Value>,
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
    /// A write that answers nothing but its failure.
    Saved(Result<(), String>),
}

pub fn run<F: Fetch>(store: &Store, fetch: &mut F, job: ExtensionJob) -> JobResult {
    let text = |error: &dyn std::fmt::Display| error.to_string();
    match job {
        ExtensionJob::Load => {
            let mut state = store.load();
            let installed = (state.installed.iter())
                .map(|installed| Loaded {
                    module: store.module_path(installed),
                    has_module: store.has_module(installed),
                    config: (state.configs.remove(&installed.key))
                        .unwrap_or_else(|| installed.current.manifest.default_config()),
                    storage: state.kv.remove(&installed.key).unwrap_or_default(),
                    installed: installed.clone(),
                })
                .collect();
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
    }
}

fn loaded(store: &Store, installed: Installed) -> Result<Loaded, String> {
    let manifest = &installed.current.manifest;
    Ok(Loaded {
        module: store.module_path(&installed),
        has_module: store.has_module(&installed),
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
