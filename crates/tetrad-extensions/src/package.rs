//! The `.tetradext` package: one UTF-8 JSON document.
//!
//! - Format 1: `{format: 1, manifest, module?}`, one bundled, self-contained ES module.
//! - Format 2: `{format: 2, manifest, module?, blocks?}`. `blocks` maps each static block ID
//!   of the manifest to its qcdsp IR.
//!
//! - Format 3: `{format: 3, manifest, module?, blocks?, remotePage}`. `remotePage` is the HTML of
//!   the extension's Remote tab ([`MAX_PAGE`] bytes at most); the manifest's `remote` needs it.
//! - Format 4: `{format: 4, manifest, module?, blocks?, remotePage?, appFiles}`. `appFiles` pins
//!   each file of the manifest's `app` by name, size and SHA-256. The files are release assets
//!   next to the package ([`Package::app_asset_name`]), so a large program does not make the
//!   package large; the signed catalog pins the package, and the package pins the files.
//!
//! A data-only package ([`Manifest::data_only`]: themes or static blocks, no code) has no
//! module. Every IR is checked with qcdsp when a package is decoded or encoded.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::manifest::{Manifest, ManifestError};

/// The format [`Package::encode`] writes for a package with blocks.
pub const PACKAGE_FORMAT: u32 = 2;
/// The format of a package with a Remote page.
pub const PACKAGE_FORMAT_PAGE: u32 = 3;
/// Largest Remote page in bytes.
pub const MAX_PAGE: usize = 256 * 1024;
/// The format of a package with a program.
pub const PACKAGE_FORMAT_APP: u32 = 4;
/// Most program files of one package.
pub const MAX_APP_FILES: usize = 16;
/// Largest program file in bytes.
pub const MAX_APP_FILE: u64 = 32 * 1024 * 1024;
/// Largest sum of the program files in bytes.
pub const MAX_APP_BYTES: u64 = 64 * 1024 * 1024;
/// The format of a package without blocks, which every host reads.
pub const PACKAGE_FORMAT_NO_BLOCKS: u32 = 1;
/// Largest package in bytes. The IR of a large block can take most of it.
pub const MAX_PACKAGE: usize = 4 * 1024 * 1024;
pub const PACKAGE_SUFFIX: &str = ".tetradext";

#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub manifest: Manifest,
    /// ES module source whose default export has `activate(ctx)`. `None` only in a
    /// data-only package.
    pub module: Option<String>,
    /// The validated IR of every static block, by block ID.
    pub blocks: BTreeMap<String, qcdsp::Block>,
    /// The HTML of the Remote tab (`remote.html`); `Some` exactly when the manifest has `remote`.
    pub remote_page: Option<String>,
    /// The program files, in name order; not empty exactly when the manifest has `app`.
    pub app_files: Vec<AppFile>,
}

/// One program file, pinned by size and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppFile {
    /// [`crate::manifest::file_name_ok`].
    pub name: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    format: u32,
    manifest: Manifest,
    #[serde(default)]
    module: Option<String>,
    /// Decoded one block at a time, so an error names its block.
    #[serde(default)]
    blocks: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    remote_page: Option<String>,
    #[serde(default)]
    app_files: Vec<AppFile>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvelopeRef<'a> {
    format: u32,
    manifest: &'a Manifest,
    #[serde(skip_serializing_if = "Option::is_none")]
    module: Option<&'a str>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    blocks: &'a BTreeMap<String, qcdsp::Block>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_page: Option<&'a str>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    app_files: &'a [AppFile],
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
        "the package has no module source (only a package of themes or static blocks without \
         settings, commands or capabilities can omit it)"
    )]
    EmptyModule,
    #[error("format 1 packages cannot hold blocks")]
    BlocksNeedFormat2,
    #[error("block {0:?}: {1}")]
    Block(String, String),
    #[error("a remote page needs a package of format {PACKAGE_FORMAT_PAGE}")]
    PageNeedsFormat3,
    #[error("the manifest has a remote tab but the package has no page, or the reverse")]
    PageMismatch,
    #[error("the remote page is over the {MAX_PAGE} byte limit")]
    PageTooLarge,
    #[error("program files need a package of format {PACKAGE_FORMAT_APP}")]
    AppNeedsFormat4,
    #[error("program files: {0}")]
    AppFiles(&'static str),
}

impl Package {
    pub fn decode(bytes: &[u8]) -> Result<Self, PackageError> {
        if bytes.len() > MAX_PACKAGE {
            return Err(PackageError::TooLarge(bytes.len()));
        }
        let envelope: Envelope = serde_json::from_slice(bytes)?;
        match envelope.format {
            PACKAGE_FORMAT_NO_BLOCKS if !envelope.blocks.is_empty() => {
                return Err(PackageError::BlocksNeedFormat2);
            }
            PACKAGE_FORMAT_NO_BLOCKS
            | PACKAGE_FORMAT
            | PACKAGE_FORMAT_PAGE
            | PACKAGE_FORMAT_APP => {}
            other => return Err(PackageError::Format(other)),
        }
        if envelope.format < PACKAGE_FORMAT_PAGE && envelope.remote_page.is_some() {
            return Err(PackageError::PageNeedsFormat3);
        }
        if envelope.format < PACKAGE_FORMAT_APP && !envelope.app_files.is_empty() {
            return Err(PackageError::AppNeedsFormat4);
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
            remote_page: envelope.remote_page,
            app_files: envelope.app_files,
        };
        package.validate()?;
        Ok(package)
    }

    pub fn encode(&self) -> Result<Vec<u8>, PackageError> {
        self.validate()?;
        let format = if !self.app_files.is_empty() {
            PACKAGE_FORMAT_APP
        } else if self.remote_page.is_some() {
            PACKAGE_FORMAT_PAGE
        } else if self.blocks.is_empty() {
            PACKAGE_FORMAT_NO_BLOCKS
        } else {
            PACKAGE_FORMAT
        };
        let bytes = serde_json::to_vec(&EnvelopeRef {
            format,
            manifest: &self.manifest,
            module: self.module.as_deref(),
            blocks: &self.blocks,
            remote_page: self.remote_page.as_deref(),
            app_files: &self.app_files,
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

    /// The release asset of one program file: the package's asset name without
    /// [`PACKAGE_SUFFIX`], a dot and the file name, such as `tetrad.doom-0.1.0.qcdoom`.
    pub fn app_asset_name(package_asset: &str, file: &str) -> String {
        let stem = package_asset
            .strip_suffix(PACKAGE_SUFFIX)
            .unwrap_or(package_asset);
        format!("{stem}.{file}")
    }

    fn validate(&self) -> Result<(), PackageError> {
        let manifest = &self.manifest;
        manifest.validate()?;
        match &self.module {
            Some(module) if module.trim().is_empty() => return Err(PackageError::EmptyModule),
            Some(_) => {}
            None if manifest.data_only() || manifest.app_only() => {}
            None => return Err(PackageError::EmptyModule),
        }
        self.validate_app_files()?;
        match (&manifest.remote, &self.remote_page) {
            (Some(_), Some(page)) if page.len() > MAX_PAGE => {
                return Err(PackageError::PageTooLarge);
            }
            (Some(_), Some(page)) if !page.trim().is_empty() => {}
            (None, None) => {}
            _ => return Err(PackageError::PageMismatch),
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

    fn validate_app_files(&self) -> Result<(), PackageError> {
        let fail = |reason| Err(PackageError::AppFiles(reason));
        let Some(app) = &self.manifest.app else {
            return match self.app_files.is_empty() {
                true => Ok(()),
                false => fail("the manifest has no app"),
            };
        };
        if self.app_files.len() > MAX_APP_FILES {
            return fail("at most 16 files");
        }
        let mut total = 0u64;
        for (i, file) in self.app_files.iter().enumerate() {
            if !crate::manifest::file_name_ok(&file.name) {
                return fail(
                    "a name must be 1..=64 characters of [A-Za-z0-9._-], not starting with '.'",
                );
            }
            // Its release asset would take or look like a package's name.
            if file.name == PACKAGE_SUFFIX[1..] || file.name.ends_with(PACKAGE_SUFFIX) {
                return fail("a name cannot end in tetradext");
            }
            if self.app_files[..i].iter().any(|f| f.name == file.name) {
                return fail("a name is listed twice");
            }
            let hex = |c: u8| c.is_ascii_digit() || (b'a'..=b'f').contains(&c);
            if file.sha256.len() != 64 || !file.sha256.bytes().all(hex) {
                return fail("a SHA-256 must be 64 lowercase hex digits");
            }
            if file.size > MAX_APP_FILE {
                return fail("a file is over 32 MiB");
            }
            total += file.size;
        }
        if total > MAX_APP_BYTES {
            return fail("the files are over 64 MiB together");
        }
        if !self.app_files.iter().any(|f| f.name == app.exec) {
            return fail("the program file (exec) is not in the package");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestError;
    use serde_json::{Value, json};

    fn package() -> Package {
        Package {
            manifest: serde_json::from_value(json!({
                "id": "tetrad.test", "name": "Test", "version": "0.1.0", "api": 1
            }))
            .unwrap(),
            module: Some("export default { activate() {} };".into()),
            blocks: BTreeMap::new(),
            remote_page: None,
            app_files: Vec::new(),
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
            remote_page: None,
            app_files: Vec::new(),
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
        v["format"] = json!(5);
        assert!(matches!(decode(&v), Err(PackageError::Format(5))));
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
        refuse(&|v| v["blocks"]["gain"]["version"] = json!(4));
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
                assert!(package.blocks.is_empty());
            }
            decoded += 1;
        }
        assert!(decoded > 0, "no packages in {}", dir.display());
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
            module: None,
            blocks: BTreeMap::new(),
            remote_page: None,
            app_files: Vec::new(),
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
        assert_eq!(pack.module, None);
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

    /// A manifest with a Remote tab: both the capability and the title, API 6, and the page in
    /// a format 3 package.
    fn tab_manifest() -> Manifest {
        serde_json::from_value(json!({
            "id": "tetrad.tabbed", "name": "Tabbed", "version": "1.0.0", "api": 6,
            "capabilities": ["remote"], "remote": {"title": "Models"}
        }))
        .unwrap()
    }

    #[test]
    fn a_remote_page_makes_a_format_3_package_and_round_trips() {
        let pack = Package {
            manifest: tab_manifest(),
            module: Some("export default { activate() {} };".into()),
            blocks: BTreeMap::new(),
            remote_page: Some("<p>hi</p>".into()),
            app_files: Vec::new(),
        };
        let bytes = pack.encode().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["format"],
            3
        );
        assert_eq!(Package::decode(&bytes).unwrap(), pack);
        // A package without a page stays format 1 for hosts that know nothing of pages.
        assert_eq!(envelope(&package())["format"], 1);
    }

    #[test]
    fn a_remote_tab_and_its_page_go_together() {
        let module = Some("export default { activate() {} };".to_string());
        let mut pack = Package {
            manifest: tab_manifest(),
            module,
            blocks: BTreeMap::new(),
            remote_page: None,
            app_files: Vec::new(),
        };
        assert!(matches!(pack.encode(), Err(PackageError::PageMismatch)));
        pack.remote_page = Some("   ".into());
        assert!(matches!(pack.encode(), Err(PackageError::PageMismatch)));
        pack.remote_page = Some("x".repeat(MAX_PAGE + 1));
        assert!(matches!(pack.encode(), Err(PackageError::PageTooLarge)));
        // A page without the tab in the manifest.
        let mut plain = package();
        plain.remote_page = Some("<p>hi</p>".into());
        assert!(matches!(plain.encode(), Err(PackageError::PageMismatch)));
        // A page in an older format is refused when decoded.
        let mut value = envelope(&Package {
            remote_page: Some("<p>hi</p>".into()),
            ..Package {
                manifest: tab_manifest(),
                module: Some("export default { activate() {} };".into()),
                blocks: BTreeMap::new(),
                remote_page: Some("<p>hi</p>".into()),
                app_files: Vec::new(),
            }
        });
        value["format"] = json!(2);
        assert!(matches!(
            Package::decode(&serde_json::to_vec(&value).unwrap()),
            Err(PackageError::PageNeedsFormat3)
        ));
    }

    fn doom() -> Package {
        Package {
            manifest: serde_json::from_value(json!({
                "id": "tetrad.doom", "name": "DOOM", "version": "0.1.0", "api": 7,
                "app": {"label": "Play DOOM", "exec": "qcdoom"}
            }))
            .unwrap(),
            module: None,
            blocks: BTreeMap::new(),
            remote_page: None,
            app_files: vec![AppFile {
                name: "qcdoom".into(),
                size: 3,
                sha256: crate::catalog::sha256_hex(b"elf"),
            }],
        }
    }

    /// A program needs no module, makes a format 4 package and pins its files; the program
    /// file must be among them, and files without a program are refused.
    #[test]
    fn a_program_package_pins_its_files() {
        let pack = doom();
        let v = envelope(&pack);
        assert_eq!(v["format"], 4);
        assert_eq!(decode(&v).unwrap(), pack);
        assert_eq!(
            Package::app_asset_name("tetrad.doom-0.1.0.tetradext", "doom1.wad"),
            "tetrad.doom-0.1.0.doom1.wad"
        );
        let refused = |edit: &dyn Fn(&mut Package)| {
            let mut bad = doom();
            edit(&mut bad);
            assert!(
                matches!(bad.encode(), Err(PackageError::AppFiles(_))),
                "{bad:?}"
            );
        };
        refused(&|p| p.app_files[0].name = "other".into());
        // A second file with a bad name: one that leaves the folder, or whose release asset
        // would take or look like a package's name.
        for name in ["../qcdoom", "tetradext", "data.tetradext"] {
            refused(&|p| {
                let extra = AppFile {
                    name: name.into(),
                    ..p.app_files[0].clone()
                };
                p.app_files.push(extra)
            });
        }
        refused(&|p| p.app_files[0].sha256 = "AB".repeat(32));
        refused(&|p| p.app_files[0].size = MAX_APP_FILE + 1);
        refused(&|p| p.app_files.push(p.app_files[0].clone()));
        refused(&|p| p.app_files.clear());
        let mut stray = package();
        stray.app_files = doom().app_files;
        assert!(matches!(stray.encode(), Err(PackageError::AppFiles(_))));
        let mut old = v.clone();
        old["format"] = json!(3);
        assert!(matches!(decode(&old), Err(PackageError::AppNeedsFormat4)));
    }
}
