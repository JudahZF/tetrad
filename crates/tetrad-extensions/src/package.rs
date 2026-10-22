//! The `.tetradext` package: one UTF-8 JSON document with the manifest and one bundled,
//! self-contained ES module. A data-only package ([`Manifest::data_only`]) may leave the
//! module out.

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
    /// ES module source whose default export has `activate(ctx)`; empty in a data-only
    /// package.
    pub module: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: u32,
    manifest: Manifest,
    #[serde(default)]
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
    #[error("the package has no module source, and its manifest is not data-only")]
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
        if self.module.trim().is_empty() && !self.manifest.data_only() {
            return Err(PackageError::EmptyModule);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestError;
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

    /// The theme pack fixture, the example in docs/extensions.md, packs without a module.
    #[test]
    fn theme_pack_fixture_packs() {
        let manifest: Manifest = serde_json::from_str(include_str!(
            "../../../extensions/fixtures/theme-pack/tetrad-extension.json"
        ))
        .unwrap();
        assert!(manifest.data_only());
        let pack = Package {
            manifest,
            module: String::new(),
        };
        assert_eq!(Package::decode(&pack.encode().unwrap()).unwrap(), pack);
    }

    /// A theme pack can never run code or ask for access: without a module it must add
    /// themes and nothing else, and a bad theme fails the whole package.
    #[test]
    fn data_only_packages_carry_themes_and_nothing_else() {
        let decode = |manifest: serde_json::Value| {
            let bytes = serde_json::to_vec(&json!({ "format": 1, "manifest": manifest })).unwrap();
            Package::decode(&bytes)
        };
        let dusk = json!({ "id": "dusk", "name": "Dusk", "colors": { "accent": "#ff8fab" } });
        let base = json!({ "id": "judah.dusk-pack", "name": "Dusk", "version": "1.0.0", "api": 1 });
        let with = |extra: serde_json::Value| {
            let mut manifest = base.clone();
            manifest
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            manifest
        };

        let pack = decode(with(json!({ "themes": [dusk] }))).unwrap();
        assert!(pack.manifest.data_only());
        assert_eq!(pack.module, "");
        assert_eq!(Package::decode(&pack.encode().unwrap()).unwrap(), pack);

        let capability = with(json!({ "themes": [dusk], "capabilities": ["midiSend"] }));
        assert!(matches!(decode(capability), Err(PackageError::EmptyModule)));
        assert!(matches!(
            decode(base.clone()),
            Err(PackageError::EmptyModule)
        ));
        let light = json!({ "id": "day", "name": "Day", "colors": { "background": "#ffffff" } });
        assert!(matches!(
            decode(with(json!({ "themes": [light] }))),
            Err(PackageError::Manifest(ManifestError::Theme(..)))
        ));
        assert!(matches!(
            decode(with(json!({ "themes": [dusk, dusk] }))),
            Err(PackageError::Manifest(ManifestError::Duplicate(_)))
        ));
    }
}
