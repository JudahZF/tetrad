//! The `.tetradext` package: one UTF-8 JSON document.
//!
//! - Format 1: `{format: 1, manifest, module}`, one bundled, self-contained ES module.
//! - Format 2: `{format: 2, manifest, module?, blocks?}`. `blocks` maps each static block ID
//!   of the manifest to its qcdsp IR. A blocks-only package (static blocks; no settings,
//!   commands, capabilities or generated blocks) has no module.
//!
//! Every IR is checked with qcdsp when a package is decoded or encoded.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::manifest::{Manifest, ManifestError};

/// The format [`Package::encode`] writes for a package with blocks or without a module.
pub const PACKAGE_FORMAT: u32 = 2;
/// The format of a package with a module and no blocks, which every host reads.
pub const PACKAGE_FORMAT_MODULE_ONLY: u32 = 1;
/// Largest package in bytes. The IR of a large block can take most of it.
pub const MAX_PACKAGE: usize = 4 * 1024 * 1024;
pub const PACKAGE_SUFFIX: &str = ".tetradext";

#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    /// ES module source whose default export has `activate(ctx)`. `None` only in a
    /// blocks-only package.
    pub module: Option<String>,
    /// The validated IR of every static block, by block ID.
    pub blocks: BTreeMap<String, qcdsp::Block>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: u32,
    manifest: Manifest,
    #[serde(default)]
    module: Option<String>,
    /// Decoded one block at a time, so an error names its block.
    #[serde(default)]
    blocks: BTreeMap<String, serde_json::Value>,
}

#[derive(Serialize)]
struct EnvelopeRef<'a> {
    format: u32,
    manifest: &'a Manifest,
    #[serde(skip_serializing_if = "Option::is_none")]
    module: Option<&'a str>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    blocks: &'a BTreeMap<String, qcdsp::Block>,
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
    #[error(
        "the package has no module source (only a package of static blocks without settings, \
         commands or capabilities can omit it)"
    )]
    EmptyModule,
    #[error("format 1 packages cannot hold blocks")]
    BlocksNeedFormat2,
    #[error("block {0:?}: {1}")]
    Block(String, String),
}

impl Package {
    pub fn decode(bytes: &[u8]) -> Result<Self, PackageError> {
        if bytes.len() > MAX_PACKAGE {
            return Err(PackageError::TooLarge(bytes.len()));
        }
        let envelope: Envelope = serde_json::from_slice(bytes)?;
        match envelope.format {
            PACKAGE_FORMAT_MODULE_ONLY if !envelope.blocks.is_empty() => {
                return Err(PackageError::BlocksNeedFormat2);
            }
            PACKAGE_FORMAT_MODULE_ONLY | PACKAGE_FORMAT => {}
            other => return Err(PackageError::Format(other)),
        }
        let blocks = envelope
            .blocks
            .into_iter()
            .map(|(id, ir)| {
                let block = serde_json::from_value::<qcdsp::Block>(ir)
                    .map_err(|e| PackageError::Block(id.clone(), e.to_string()))?;
                Ok((id, block))
            })
            .collect::<Result<_, PackageError>>()?;
        let package = Self {
            manifest: envelope.manifest,
            module: envelope.module.filter(|m| !m.trim().is_empty()),
            blocks,
        };
        package.validate()?;
        Ok(package)
    }

    pub fn encode(&self) -> Result<Vec<u8>, PackageError> {
        self.validate()?;
        let format = if self.blocks.is_empty() && self.module.is_some() {
            PACKAGE_FORMAT_MODULE_ONLY
        } else {
            PACKAGE_FORMAT
        };
        let bytes = serde_json::to_vec(&EnvelopeRef {
            format,
            manifest: &self.manifest,
            module: self.module.as_deref(),
            blocks: &self.blocks,
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
        let manifest = &self.manifest;
        manifest.validate()?;
        match &self.module {
            Some(module) if module.trim().is_empty() => return Err(PackageError::EmptyModule),
            Some(_) => {}
            None => {
                let blocks_only = !manifest.blocks.is_empty()
                    && manifest.capabilities.is_empty()
                    && manifest.settings.is_empty()
                    && manifest.commands.is_empty()
                    && manifest.blocks.iter().all(|b| !b.generated);
                if !blocks_only {
                    return Err(PackageError::EmptyModule);
                }
            }
        }
        let fail = |id: &str, reason: &str| Err(PackageError::Block(id.into(), reason.into()));
        for spec in &manifest.blocks {
            if !spec.generated && !self.blocks.contains_key(&spec.id) {
                return fail(&spec.id, "a static block needs its IR in the package");
            }
        }
        for (id, block) in &self.blocks {
            match manifest.block(id) {
                None => return fail(id, "IR for a block the manifest does not declare"),
                Some(spec) if spec.generated => {
                    return fail(id, "a generated block cannot have IR in the package");
                }
                Some(_) => {}
            }
            block
                .validate()
                .map_err(|e| PackageError::Block(id.clone(), e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn package() -> Package {
        Package {
            manifest: serde_json::from_value(json!({
                "id": "tetrad.test", "name": "Test", "version": "0.1.0", "api": 1
            }))
            .unwrap(),
            module: Some("export default { activate() {} };".into()),
            blocks: BTreeMap::new(),
        }
    }

    /// A blocks-only package with one static block.
    fn blocks_only() -> Package {
        Package {
            manifest: serde_json::from_value(json!({
                "id": "tetrad.blocks", "name": "Blocks", "version": "1.0.0", "api": 2,
                "blocks": [{"id": "gain", "name": "Gain", "category": "utility"}]
            }))
            .unwrap(),
            module: None,
            blocks: BTreeMap::from([("gain".into(), qcdsp::examples::gain().unwrap())]),
        }
    }

    fn envelope(p: &Package) -> Value {
        serde_json::from_slice(&p.encode().unwrap()).unwrap()
    }

    fn decode(v: &Value) -> Result<Package, PackageError> {
        Package::decode(&serde_json::to_vec(v).unwrap())
    }

    #[test]
    fn round_trip_and_bounds() {
        let p = package();
        let bytes = p.encode().unwrap();
        assert_eq!(
            envelope(&p)["format"],
            json!(1),
            "a module-only package stays format 1"
        );
        assert_eq!(Package::decode(&bytes).unwrap().encode().unwrap(), bytes);
        assert_eq!(p.file_name(), "tetrad.test-0.1.0.tetradext");

        let huge = vec![b' '; MAX_PACKAGE + 1];
        assert!(matches!(
            Package::decode(&huge),
            Err(PackageError::TooLarge(_))
        ));
        let mut v = envelope(&p);
        v["format"] = json!(3);
        assert!(matches!(decode(&v), Err(PackageError::Format(3))));
        let empty = Package {
            module: Some(" \n".into()),
            ..p.clone()
        };
        assert!(matches!(empty.encode(), Err(PackageError::EmptyModule)));
        let missing = Package { module: None, ..p };
        assert!(matches!(missing.encode(), Err(PackageError::EmptyModule)));
    }

    #[test]
    fn blocks_only_package_round_trips() {
        let p = blocks_only();
        let v = envelope(&p);
        assert_eq!(v["format"], json!(2));
        assert!(v.get("module").is_none());
        let decoded = decode(&v).unwrap();
        assert!(decoded.module.is_none());
        assert_eq!(decoded.blocks["gain"].parameters()[0].name, "Gain");
        assert_eq!(envelope(&decoded), v);

        let mut with_empty_module = v.clone();
        with_empty_module["module"] = json!("");
        assert!(decode(&with_empty_module).unwrap().module.is_none());
    }

    #[test]
    fn block_ir_must_match_the_manifest() {
        let v = envelope(&blocks_only());
        let refuse = |edit: &dyn Fn(&mut Value)| {
            let mut v = v.clone();
            edit(&mut v);
            decode(&v).err().unwrap_or_else(|| panic!("accepted {v}"))
        };
        // Static block without IR; IR of an unknown block; IR that qcdsp refuses.
        refuse(&|v| v["blocks"] = json!({}));
        refuse(&|v| v["blocks"]["other"] = v["blocks"]["gain"].clone());
        refuse(&|v| v["blocks"]["gain"]["output"] = json!(99));
        refuse(&|v| v["blocks"]["gain"]["version"] = json!(3));
        // Format 1 cannot carry blocks.
        let error = refuse(&|v| {
            v["format"] = json!(1);
            v["module"] = json!("export default { activate() {} };");
        });
        assert!(matches!(error, PackageError::BlocksNeedFormat2));
        // A generated block needs a module and the capability, and has no IR in the package.
        refuse(&|v| {
            v["manifest"]["capabilities"] = json!(["blocks"]);
            v["manifest"]["blocks"][0]["generated"] = json!(true);
        });
        refuse(&|v| {
            v["manifest"]["capabilities"] = json!(["blocks"]);
            v["manifest"]["blocks"][0]["generated"] = json!(true);
            v["module"] = json!("export default { activate() {} };");
        });
        // A module-less package cannot have settings, commands or capabilities.
        refuse(&|v| v["manifest"]["capabilities"] = json!(["storage"]));
        refuse(&|v| v["manifest"]["commands"] = json!([{"id": "go", "label": "Go"}]));
    }

    #[test]
    fn published_default_packages_still_decode() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../extensions/repository/packages");
        let mut decoded = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if !path.to_string_lossy().ends_with(PACKAGE_SUFFIX) {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let package =
                Package::decode(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            // API 1 packages keep their format 1 shape; API 2 packages may have blocks.
            if package.manifest.api == 1 {
                assert!(package.module.is_some() && package.blocks.is_empty());
            }
            decoded += 1;
        }
        assert!(decoded > 0, "no packages in {}", dir.display());
    }
}
