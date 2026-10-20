//! The `.tetradext` package: one UTF-8 JSON document with the manifest and one bundled,
//! self-contained ES module.

use serde::{Deserialize, Serialize};

use crate::manifest::{Manifest, ManifestError};

pub const PACKAGE_FORMAT: u32 = 1;
/// Largest package in bytes: 64 KiB under the device HTTP client's 1 MiB output limit, which
/// also holds the status line.
pub const MAX_PACKAGE: usize = 960 * 1024;
pub const PACKAGE_SUFFIX: &str = ".tetradext";

#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub manifest: Manifest,
    /// ES module source whose default export has `activate(ctx)`.
    pub module: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: u32,
    manifest: Manifest,
    module: String,
}

#[derive(Serialize)]
struct EnvelopeRef<'a> {
    format: u32,
    manifest: &'a Manifest,
    module: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("package of {0} bytes is over the {MAX_PACKAGE} byte limit")]
    TooLarge(usize),
    #[error("package format {0} is not supported")]
    Format(u32),
    #[error("bad package: {0}")]
    Json(#[from] serde_json::Error),
    #[error("manifest: {0}")]
    Manifest(#[from] ManifestError),
    #[error("the package has no module source")]
    EmptyModule,
}

impl Package {
    pub fn decode(bytes: &[u8]) -> Result<Self, PackageError> {
        if bytes.len() > MAX_PACKAGE {
            return Err(PackageError::TooLarge(bytes.len()));
        }
        let envelope: Envelope = serde_json::from_slice(bytes)?;
        if envelope.format != PACKAGE_FORMAT {
            return Err(PackageError::Format(envelope.format));
        }
        let package = Self {
            manifest: envelope.manifest,
            module: envelope.module,
        };
        package.validate()?;
        Ok(package)
    }

    pub fn encode(&self) -> Result<Vec<u8>, PackageError> {
        self.validate()?;
        let bytes = serde_json::to_vec(&EnvelopeRef {
            format: PACKAGE_FORMAT,
            manifest: &self.manifest,
            module: &self.module,
        })?;
        if bytes.len() > MAX_PACKAGE {
            return Err(PackageError::TooLarge(bytes.len()));
        }
        Ok(bytes)
    }

    /// The release asset name, `<id>-<version>.tetradext`.
    pub fn file_name(&self) -> String {
        format!(
            "{}-{}{PACKAGE_SUFFIX}",
            self.manifest.id, self.manifest.version
        )
    }

    fn validate(&self) -> Result<(), PackageError> {
        self.manifest.validate()?;
        if self.module.trim().is_empty() {
            return Err(PackageError::EmptyModule);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn package() -> Package {
        Package {
            manifest: serde_json::from_value(json!({
                "id": "tetrad.test", "name": "Test", "version": "0.1.0", "api": 1
            }))
            .unwrap(),
            module: "export default { activate() {} };".into(),
        }
    }

    #[test]
    fn round_trip_and_bounds() {
        let p = package();
        let bytes = p.encode().unwrap();
        assert_eq!(Package::decode(&bytes).unwrap(), p);
        assert_eq!(p.file_name(), "tetrad.test-0.1.0.tetradext");

        let huge = vec![b' '; MAX_PACKAGE + 1];
        assert!(matches!(
            Package::decode(&huge),
            Err(PackageError::TooLarge(_))
        ));
        let mut v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        v["format"] = json!(2);
        assert!(matches!(
            Package::decode(&serde_json::to_vec(&v).unwrap()),
            Err(PackageError::Format(2))
        ));
        let empty = Package {
            module: " \n".into(),
            ..p
        };
        assert!(matches!(empty.encode(), Err(PackageError::EmptyModule)));
    }
}
