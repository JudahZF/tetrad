//! GitHub extension sources, without I/O. The device implements [`Fetch`] over its HTTP
//! client; everything here only builds URLs and checks what comes back.
//!
//! A source publishes its catalog files on the default branch and its packages as release
//! assets. The key is fetched once, when the user adds and trusts the source; every later
//! refresh verifies against that pinned key.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::catalog::{
    self, CATALOG_FILE, Catalog, CatalogEntry, CatalogError, KEY_FILE, MAX_CATALOG, MAX_KEY_FILE,
    PublicKey, SIGNATURE_FILE,
};

/// The bundled default repository.
pub const DEFAULT_SOURCE: &str = "JudahZF/tetrad-extensions";
/// The default repository's signing key, base64.
pub const DEFAULT_KEY: &str = "DqK96SpLHkAUuJ80pbsc0sd/08qnig+reI6yqAwtbYY=";

/// A GitHub repository, `owner/repo`. Compares case-insensitively, as GitHub does, and keeps
/// the spelling it was given for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SourceId {
    owner: String,
    repo: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a GitHub repository (owner/repo or https://github.com/owner/repo)")]
pub struct SourceIdError(String);

impl SourceId {
    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }

    pub fn is_default(&self) -> bool {
        DEFAULT_SOURCE.parse::<Self>().is_ok_and(|d| &d == self)
    }

    /// Lowercase `owner_repo`. Unique per source because owners have no `_`.
    pub fn dir_name(&self) -> String {
        format!("{}_{}", self.owner, self.repo).to_ascii_lowercase()
    }

    /// A catalog file on the default branch.
    pub fn raw_url(&self, file: &str) -> String {
        format!(
            "https://raw.githubusercontent.com/{}/{}/HEAD/{file}",
            self.owner, self.repo
        )
    }

    pub fn asset_url(&self, asset: &catalog::Asset) -> String {
        format!(
            "https://github.com/{}/{}/releases/download/{}/{}",
            self.owner, self.repo, asset.tag, asset.name
        )
    }
}

impl FromStr for SourceId {
    type Err = SourceIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let fail = || SourceIdError(text.to_owned());
        let trimmed = text.trim();
        let path = ["https://github.com/", "https://www.github.com/"]
            .iter()
            .find_map(|prefix| strip_prefix_ignore_case(trimmed, prefix))
            .unwrap_or(trimmed);
        let path = path.strip_suffix('/').unwrap_or(path);
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, repo) = path.split_once('/').ok_or_else(fail)?;
        if !owner_ok(owner) || !repo_ok(repo) {
            return Err(fail());
        }
        Ok(Self {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
        })
    }
}

impl TryFrom<String> for SourceId {
    type Error = SourceIdError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl From<SourceId> for String {
    fn from(source: SourceId) -> Self {
        source.to_string()
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

impl PartialEq for SourceId {
    fn eq(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.repo.eq_ignore_ascii_case(&other.repo)
    }
}

impl Eq for SourceId {}

impl Hash for SourceId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.dir_name().hash(state);
    }
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// GitHub user and organization names: 1..=39 of `[A-Za-z0-9-]`, no edge hyphen.
fn owner_ok(owner: &str) -> bool {
    (1..=39).contains(&owner.len())
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// GitHub repository names: 1..=100 of `[A-Za-z0-9._-]`, not `.` or `..`.
fn repo_ok(repo: &str) -> bool {
    (1..=100).contains(&repo.len())
        && repo != "."
        && repo != ".."
        && repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// An HTTPS GET. Implementations follow redirects, verify TLS and stop reading past
/// `max_bytes`.
pub trait Fetch {
    fn get(&mut self, url: &str, max_bytes: usize) -> Result<Vec<u8>, FetchError>;
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("not found")]
    NotFound,
    #[error("response is over {0} bytes")]
    TooLarge(usize),
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("{url}: {error}")]
    Fetch { url: String, error: FetchError },
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error("{0}: size does not match the signed catalog")]
    Size(String),
    #[error("{0}: SHA-256 does not match the signed catalog")]
    Hash(String),
}

/// A verified catalog with the exact bytes it was verified from, for the store's cache.
#[derive(Debug, Clone)]
pub struct Refreshed {
    pub catalog: Catalog,
    pub json: Vec<u8>,
    pub signature: Vec<u8>,
}

fn get(fetch: &mut impl Fetch, url: String, max: usize) -> Result<Vec<u8>, RepositoryError> {
    match fetch.get(&url, max) {
        Ok(bytes) if bytes.len() > max => Err(RepositoryError::Fetch {
            url,
            error: FetchError::TooLarge(max),
        }),
        Ok(bytes) => Ok(bytes),
        Err(error) => Err(RepositoryError::Fetch { url, error }),
    }
}

/// The source's published key. Show its fingerprint and pin it only after the user trusts it.
pub fn fetch_key(fetch: &mut impl Fetch, source: &SourceId) -> Result<PublicKey, RepositoryError> {
    let bytes = get(fetch, source.raw_url(KEY_FILE), MAX_KEY_FILE)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| CatalogError::Key("not UTF-8"))?;
    Ok(PublicKey::from_base64(text)?)
}

/// Fetches the catalog and its signature and verifies them against the pinned key.
pub fn refresh(
    fetch: &mut impl Fetch,
    source: &SourceId,
    key: &PublicKey,
) -> Result<Refreshed, RepositoryError> {
    let json = get(fetch, source.raw_url(CATALOG_FILE), MAX_CATALOG)?;
    let signature = get(fetch, source.raw_url(SIGNATURE_FILE), MAX_KEY_FILE)?;
    let catalog = Catalog::verify(&json, &signature, key)?;
    Ok(Refreshed {
        catalog,
        json,
        signature,
    })
}

/// Fetches one package and checks it against its signed catalog entry.
pub fn download(
    fetch: &mut impl Fetch,
    source: &SourceId,
    entry: &CatalogEntry,
) -> Result<Vec<u8>, RepositoryError> {
    let url = source.asset_url(&entry.asset);
    // The catalog bounds the size at MAX_PACKAGE, so this fits in usize.
    let bytes = get(fetch, url.clone(), entry.asset.size as usize)?;
    check_asset(&bytes, entry).map_err(|e| match e {
        AssetMismatch::Size => RepositoryError::Size(url),
        AssetMismatch::Hash => RepositoryError::Hash(url),
    })?;
    Ok(bytes)
}

pub(crate) enum AssetMismatch {
    Size,
    Hash,
}

pub(crate) fn check_asset(bytes: &[u8], entry: &CatalogEntry) -> Result<(), AssetMismatch> {
    if bytes.len() as u64 != entry.asset.size {
        Err(AssetMismatch::Size)
    } else if catalog::sha256_hex(bytes) != entry.asset.sha256 {
        Err(AssetMismatch::Hash)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_parse_and_compare_like_github() {
        let canonical: SourceId = "JudahZF/tetrad-extensions".parse().unwrap();
        for form in [
            "judahzf/Tetrad-Extensions",
            "https://github.com/JudahZF/tetrad-extensions",
            "https://github.com/JudahZF/tetrad-extensions/",
            "https://GitHub.com/JudahZF/tetrad-extensions.git",
            " https://www.github.com/JudahZF/tetrad-extensions.git/ ",
        ] {
            assert_eq!(form.parse::<SourceId>().unwrap(), canonical, "{form}");
        }
        assert!(canonical.is_default());
        assert_eq!(canonical.to_string(), "JudahZF/tetrad-extensions");
        assert_eq!(canonical.dir_name(), "judahzf_tetrad-extensions");
        for bad in [
            "owner",
            "a/b/c",
            "-a/b",
            "a/..",
            "a b/c",
            "https://gitlab.com/a/b",
            "http://github.com/a/b",
            "a/b?x",
        ] {
            assert!(bad.parse::<SourceId>().is_err(), "{bad}");
        }
    }

    #[test]
    fn urls() {
        let source: SourceId = "o/r".parse().unwrap();
        assert_eq!(
            source.raw_url(CATALOG_FILE),
            "https://raw.githubusercontent.com/o/r/HEAD/tetrad-extensions.json"
        );
        let asset = catalog::Asset {
            tag: "t1".into(),
            name: "a.tetradext".into(),
            size: 1,
            sha256: String::new(),
        };
        assert_eq!(
            source.asset_url(&asset),
            "https://github.com/o/r/releases/download/t1/a.tetradext"
        );
    }

    #[test]
    fn default_key_is_valid() {
        PublicKey::from_base64(DEFAULT_KEY).unwrap();
    }
}
