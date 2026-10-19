//! The signed repository catalog: `tetrad-extensions.json`, its detached Ed25519 signature
//! `tetrad-extensions.sig` and the repository key `tetrad-extensions.pub`.
//!
//! The signature covers the exact catalog bytes and is checked before the JSON is parsed.
//! Each entry pins one release asset by size and SHA-256, so a verified catalog vouches for
//! every package it lists.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{self, API_VERSION, MAX_DESCRIPTION, MAX_NAME};
use crate::package::{MAX_PACKAGE, PACKAGE_SUFFIX};

pub const CATALOG_FORMAT: u32 = 1;
pub const CATALOG_FILE: &str = "tetrad-extensions.json";
pub const SIGNATURE_FILE: &str = "tetrad-extensions.sig";
pub const KEY_FILE: &str = "tetrad-extensions.pub";
/// Largest catalog in bytes.
pub const MAX_CATALOG: usize = 256 * 1024;
pub const MAX_ENTRIES: usize = 256;
/// Largest signature or key file in bytes (base64 text plus whitespace).
pub const MAX_KEY_FILE: usize = 1024;
/// Longest release tag or asset name.
const MAX_ASSET_NAME: usize = 128;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub format: u32,
    pub extensions: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub version: Version,
    pub description: String,
    pub api: u32,
    pub asset: Asset,
}

/// One GitHub release asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub tag: String,
    pub name: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the asset bytes.
    pub sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("{0} is over its size limit")]
    TooLarge(&'static str),
    #[error("bad key: {0}")]
    Key(&'static str),
    #[error("bad signature encoding")]
    SignatureEncoding,
    #[error("the catalog signature does not match the repository key")]
    BadSignature,
    #[error("bad catalog: {0}")]
    Json(#[from] serde_json::Error),
    #[error("catalog format {0} is not supported")]
    Format(u32),
    #[error("at most {MAX_ENTRIES} entries")]
    TooMany,
    #[error("entry {id} {version}: {reason}")]
    Entry {
        id: String,
        version: String,
        reason: String,
    },
}

/// A repository's Ed25519 verifying key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicKey(VerifyingKey);

impl PublicKey {
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, CatalogError> {
        VerifyingKey::from_bytes(bytes)
            .map(Self)
            .map_err(|_| CatalogError::Key("not an Ed25519 point"))
    }

    /// Accepts surrounding whitespace, as in a `.pub` file.
    pub fn from_base64(text: &str) -> Result<Self, CatalogError> {
        let bytes = BASE64
            .decode(text.trim())
            .map_err(|_| CatalogError::Key("not base64"))?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| CatalogError::Key("not 32 bytes"))?;
        Self::from_bytes(&bytes)
    }

    pub fn to_base64(&self) -> String {
        BASE64.encode(self.0.as_bytes())
    }

    /// The first 32 hex digits (128 bits) of the key's SHA-256, in groups of four: `3f2a 9c01 …`.
    pub fn fingerprint(&self) -> String {
        let hex = hex(&Sha256::digest(self.0.as_bytes())[..16]);
        let groups: Vec<&str> = (0..8).map(|i| &hex[i * 4..i * 4 + 4]).collect();
        groups.join(" ")
    }

    /// Strict Ed25519 verification of a base64 signature over `message`.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), CatalogError> {
        if signature.len() > MAX_KEY_FILE {
            return Err(CatalogError::TooLarge("signature"));
        }
        let text = std::str::from_utf8(signature).map_err(|_| CatalogError::SignatureEncoding)?;
        let bytes = BASE64
            .decode(text.trim())
            .map_err(|_| CatalogError::SignatureEncoding)?;
        let signature =
            Signature::from_slice(&bytes).map_err(|_| CatalogError::SignatureEncoding)?;
        self.0
            .verify_strict(message, &signature)
            .map_err(|_| CatalogError::BadSignature)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.fingerprint())
    }
}

impl Serialize for PublicKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_base64())
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_base64(&text).map_err(serde::de::Error::custom)
    }
}

impl Catalog {
    /// Verifies `signature` over the exact `json` bytes with `key`, then parses and checks.
    pub fn verify(json: &[u8], signature: &[u8], key: &PublicKey) -> Result<Self, CatalogError> {
        if json.len() > MAX_CATALOG {
            return Err(CatalogError::TooLarge("catalog"));
        }
        key.verify(json, signature)?;
        Self::parse_unsigned(json)
    }

    /// Parses and checks a catalog without a signature. For publishing tools only; the
    /// device always uses [`Catalog::verify`].
    pub fn parse_unsigned(json: &[u8]) -> Result<Self, CatalogError> {
        if json.len() > MAX_CATALOG {
            return Err(CatalogError::TooLarge("catalog"));
        }
        let catalog: Self = serde_json::from_slice(json)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.format != CATALOG_FORMAT {
            return Err(CatalogError::Format(self.format));
        }
        if self.extensions.len() > MAX_ENTRIES {
            return Err(CatalogError::TooMany);
        }
        for (i, entry) in self.extensions.iter().enumerate() {
            let fail = |reason: &str| CatalogError::Entry {
                id: entry.id.clone(),
                version: entry.version.to_string(),
                reason: reason.into(),
            };
            entry.validate().map_err(fail)?;
            if self.extensions[..i]
                .iter()
                .any(|e| e.id == entry.id && e.version == entry.version)
            {
                return Err(fail("listed twice"));
            }
        }
        Ok(())
    }

    /// The newest release of `id` that this host's API can run.
    pub fn latest(&self, id: &str) -> Option<&CatalogEntry> {
        self.extensions
            .iter()
            .filter(|e| e.id == id && e.api == API_VERSION)
            .max_by(|a, b| a.version.cmp(&b.version))
    }

    pub fn entry(&self, id: &str, version: &Version) -> Option<&CatalogEntry> {
        self.extensions
            .iter()
            .find(|e| e.id == id && &e.version == version)
    }
}

impl CatalogEntry {
    fn validate(&self) -> Result<(), &'static str> {
        manifest::validate_id(&self.id).map_err(|_| "bad ID")?;
        let name = self.name.chars().count();
        if !(1..=MAX_NAME).contains(&name) || self.description.chars().count() > MAX_DESCRIPTION {
            return Err("bad name or description");
        }
        if !asset_name_ok(&self.asset.tag) {
            return Err("bad release tag");
        }
        if !asset_name_ok(&self.asset.name) || !self.asset.name.ends_with(PACKAGE_SUFFIX) {
            return Err("bad asset name");
        }
        if self.asset.size == 0 || self.asset.size > MAX_PACKAGE as u64 {
            return Err("bad asset size");
        }
        let sha_ok = self.asset.sha256.len() == 64
            && self
                .asset
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !sha_ok {
            return Err("sha256 must be 64 lowercase hex digits");
        }
        Ok(())
    }
}

/// Safe in a URL path segment and a file name: `[A-Za-z0-9._-]`, not starting with `.`.
fn asset_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ASSET_NAME
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed(catalog: &Catalog) -> (Vec<u8>, Vec<u8>, PublicKey) {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let json = serde_json::to_vec(catalog).unwrap();
        let sig = BASE64.encode(signing.sign(&json).to_bytes()).into_bytes();
        let key = PublicKey::from_bytes(signing.verifying_key().as_bytes()).unwrap();
        (json, sig, key)
    }

    fn entry(version: &str, api: u32) -> CatalogEntry {
        CatalogEntry {
            id: "tetrad.test".into(),
            name: "Test".into(),
            version: version.parse().unwrap(),
            description: String::new(),
            api,
            asset: Asset {
                tag: "extensions-2026-10-19".into(),
                name: format!("tetrad.test-{version}.tetradext"),
                size: 10,
                sha256: sha256_hex(b"x"),
            },
        }
    }

    #[test]
    fn signature_is_checked_before_parsing() {
        let catalog = Catalog {
            format: 1,
            extensions: vec![entry("1.0.0", 1)],
        };
        let (json, sig, key) = signed(&catalog);
        assert_eq!(Catalog::verify(&json, &sig, &key).unwrap(), catalog);

        let mut tampered = json.clone();
        tampered[0] = b' ';
        assert!(matches!(
            Catalog::verify(&tampered, &sig, &key),
            Err(CatalogError::BadSignature)
        ));
        let other =
            PublicKey::from_bytes(SigningKey::from_bytes(&[8; 32]).verifying_key().as_bytes())
                .unwrap();
        assert!(matches!(
            Catalog::verify(&json, &sig, &other),
            Err(CatalogError::BadSignature)
        ));
        // Garbage that is correctly signed still fails as JSON, after the signature.
        let (garbage, sig, key) = {
            let signing = SigningKey::from_bytes(&[7; 32]);
            let sig = BASE64.encode(signing.sign(b"{").to_bytes()).into_bytes();
            (b"{".to_vec(), sig, key)
        };
        assert!(matches!(
            Catalog::verify(&garbage, &sig, &key),
            Err(CatalogError::Json(_))
        ));
    }

    #[test]
    fn latest_skips_unsupported_api_and_duplicates_are_refused() {
        let catalog = Catalog {
            format: 1,
            extensions: vec![entry("1.2.0", 1), entry("1.10.0", 1), entry("2.0.0", 2)],
        };
        catalog.validate().unwrap();
        assert_eq!(
            catalog.latest("tetrad.test").unwrap().version.to_string(),
            "1.10.0"
        );
        let dup = Catalog {
            format: 1,
            extensions: vec![entry("1.2.0", 1), entry("1.2.0", 1)],
        };
        assert!(matches!(dup.validate(), Err(CatalogError::Entry { .. })));
        let mut bad = entry("1.0.0", 1);
        bad.asset.name = "../x.tetradext".into();
        assert!(
            Catalog {
                format: 1,
                extensions: vec![bad]
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn key_round_trip_and_fingerprint() {
        let key =
            PublicKey::from_bytes(SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes())
                .unwrap();
        assert_eq!(
            PublicKey::from_base64(&format!(" {}\n", key.to_base64())).unwrap(),
            key
        );
        let fp = key.fingerprint();
        assert_eq!(fp.len(), 39);
        assert_eq!(fp.split(' ').count(), 8);
        assert!(PublicKey::from_base64("AAAA").is_err());
    }
}
