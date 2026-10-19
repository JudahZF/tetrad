//! `tetrad-ext`: desktop and CI tooling for Tetrad extensions: scaffold, type-check and
//! bundle, package, and sign a repository catalog.
#![forbid(unsafe_code)]

use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signer, SigningKey};
use tetrad_extensions::catalog::{
    Asset, CATALOG_FILE, CATALOG_FORMAT, Catalog, CatalogEntry, KEY_FILE, PublicKey,
    SIGNATURE_FILE, sha256_hex,
};
use tetrad_extensions::manifest::{API_VERSION, Manifest};
use tetrad_extensions::package::{PACKAGE_SUFFIX, Package};
use tetrad_extensions::sdk::SDK_DECLARATIONS;

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

const MANIFEST_FILE: &str = "tetrad-extension.json";
const BUNDLE: &str = "dist/index.mjs";
const SIGNING_KEY_FILE: &str = "signing.key";
const TYPESCRIPT_VERSION: &str = "7.0.2";
const ESBUILD_VERSION: &str = "0.28.2";

#[derive(Parser)]
#[command(
    name = "tetrad-ext",
    version,
    about = "Build, package and publish Tetrad extensions"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a repository signing key: `signing.key` (private) and `tetrad-extensions.pub`.
    Keygen {
        #[arg(long)]
        out: PathBuf,
    },
    /// Scaffold a strict TypeScript extension in a new directory.
    Init {
        #[arg(long)]
        name: String,
        #[arg(long)]
        id: String,
        dir: PathBuf,
    },
    /// Write the SDK declarations (`tetrad.d.ts`).
    Sdk {
        #[arg(long)]
        out: PathBuf,
    },
    /// Type-check with tsc, then bundle `src/index.ts` to `dist/index.mjs` with esbuild.
    Build { dir: PathBuf },
    /// Pack `tetrad-extension.json` and `dist/index.mjs` into a `.tetradext` file.
    Package {
        dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write and sign the catalog for `<repo-dir>/packages/*.tetradext`.
    Catalog {
        #[arg(long)]
        repo_dir: PathBuf,
        #[arg(long)]
        key: PathBuf,
        /// Release tag of new packages. Packages already in the catalog keep their tag.
        #[arg(long)]
        tag: String,
    },
    /// Validate a package and print its manifest.
    Check { file: PathBuf },
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Cmd::Keygen { out } => keygen(&out),
        Cmd::Init { name, id, dir } => init(&name, &id, &dir),
        Cmd::Sdk { out } => write(&out, SDK_DECLARATIONS),
        Cmd::Build { dir } => build(&dir),
        Cmd::Package { dir, out } => package(&dir, &out),
        Cmd::Catalog { repo_dir, key, tag } => catalog(&repo_dir, &key, &tag),
        Cmd::Check { file } => check(&file),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn write(path: &Path, contents: impl AsRef<[u8]>) -> Result {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn keygen(out: &Path) -> Result {
    let mut seed = [0u8; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
    let key = SigningKey::from_bytes(&seed);
    fs::create_dir_all(out)?;
    let path = out.join(SIGNING_KEY_FILE);
    // create_new: never replace a key that may already sign a published catalog.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(file, "{}", BASE64.encode(key.to_bytes()))?;
    let public = public_key(&key)?;
    write(&out.join(KEY_FILE), format!("{}\n", public.to_base64()))?;
    println!("public key  {}", public.to_base64());
    println!("fingerprint {}", public.fingerprint());
    Ok(())
}

fn read_signing_key(path: &Path) -> Result<SigningKey> {
    let text = String::from_utf8(read(path)?)?;
    let seed: [u8; 32] = BASE64
        .decode(text.trim())?
        .try_into()
        .map_err(|_| format!("{}: not a 32-byte key", path.display()))?;
    Ok(SigningKey::from_bytes(&seed))
}

fn public_key(key: &SigningKey) -> Result<PublicKey> {
    Ok(PublicKey::from_bytes(key.verifying_key().as_bytes())?)
}

fn init(name: &str, id: &str, dir: &Path) -> Result {
    let manifest: Manifest = serde_json::from_value(serde_json::json!({
        "id": id,
        "name": name,
        "version": "0.1.0",
        "description": "",
        "api": API_VERSION,
        "capabilities": ["storage"],
        "settings": [],
        "commands": [{ "id": "hello", "label": "Say hello" }],
    }))?;
    manifest.validate()?;
    if dir.exists() && fs::read_dir(dir)?.next().is_some() {
        return Err(format!("{} is not empty", dir.display()).into());
    }
    let package_json = serde_json::json!({
        "name": id,
        "private": true,
        "type": "module",
        "scripts": { "build": "tetrad-ext build .", "check": "tsc -p . --noEmit" },
        "devDependencies": { "esbuild": ESBUILD_VERSION, "typescript": TYPESCRIPT_VERSION },
    });
    let files: [(&str, String); 6] = [
        (
            MANIFEST_FILE,
            serde_json::to_string_pretty(&manifest)? + "\n",
        ),
        (
            "package.json",
            serde_json::to_string_pretty(&package_json)? + "\n",
        ),
        ("tsconfig.json", TSCONFIG.into()),
        ("tetrad.d.ts", SDK_DECLARATIONS.into()),
        ("src/index.ts", INDEX_TS.into()),
        (".gitignore", "node_modules/\ndist/\n".into()),
    ];
    for (file, contents) in files {
        write(&dir.join(file), contents)?;
    }
    println!(
        "created {}; next: cd {0} && npm install && tetrad-ext build .",
        dir.display()
    );
    Ok(())
}

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "es2020",
    "lib": ["es2020"],
    "module": "preserve",
    "moduleResolution": "bundler",
    "strict": true,
    "noEmit": true,
    "noUncheckedIndexedAccess": true,
    "exactOptionalPropertyTypes": true,
    "noImplicitReturns": true,
    "noFallthroughCasesInSwitch": true,
    "verbatimModuleSyntax": true,
    "isolatedModules": true,
    "skipLibCheck": false,
    "types": [],
    "paths": { "@tetrad/extensions": ["./tetrad.d.ts"] }
  },
  "include": ["src"]
}
"#;

const INDEX_TS: &str = r#"import type { ExtensionDefinition } from "@tetrad/extensions";

const extension: ExtensionDefinition = {
  activate(ctx) {
    ctx.commands.register("hello", async (scope) => {
      const count = await scope.storage.get("count");
      const next = typeof count === "number" ? count + 1 : 1;
      await scope.storage.set("count", next);
      await scope.ui.setStatus([`Hello ${next} times from ${ctx.extension.id}`]);
    });
  },
};

export default extension;
"#;

fn build(dir: &Path) -> Result {
    run(dir, &["tsc", "-p", ".", "--noEmit"])?;
    run(
        dir,
        &[
            "esbuild",
            "src/index.ts",
            "--bundle",
            "--format=esm",
            "--platform=neutral",
            "--target=es2020",
            &format!("--outfile={BUNDLE}"),
        ],
    )?;
    println!("built {}", dir.join(BUNDLE).display());
    Ok(())
}

/// Runs a locally installed npm tool in `dir`; on failure the error carries its output.
fn run(dir: &Path, args: &[&str]) -> Result {
    let output = Command::new("npx")
        .arg("--no-install")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("npx: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{} failed ({})\n{}{}",
        args[0],
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

fn package(dir: &Path, out: &Path) -> Result {
    let manifest: Manifest = serde_json::from_slice(&read(&dir.join(MANIFEST_FILE))?)
        .map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
    let module = String::from_utf8(read(&dir.join(BUNDLE))?)?;
    let bytes = Package { manifest, module }.encode()?;
    write(out, &bytes)?;
    println!("{} ({} bytes)", out.display(), bytes.len());
    Ok(())
}

fn catalog(repo: &Path, key_path: &Path, tag: &str) -> Result {
    let key = read_signing_key(key_path)?;
    let catalog_path = repo.join(CATALOG_FILE);
    // Packages already published keep the tag of the release that holds them.
    let previous = match fs::read(&catalog_path) {
        Ok(bytes) => Catalog::parse_unsigned(&bytes)?.extensions,
        Err(_) => Vec::new(),
    };

    let mut files: Vec<PathBuf> = fs::read_dir(repo.join("packages"))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    files.retain(|p| p.to_string_lossy().ends_with(PACKAGE_SUFFIX));
    files.sort();

    let mut extensions = Vec::new();
    for path in files {
        let bytes = read(&path)?;
        let package = Package::decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("bad file name")?
            .to_owned();
        let sha256 = sha256_hex(&bytes);
        let tag = previous
            .iter()
            .find(|e| e.asset.name == name && e.asset.sha256 == sha256)
            .map_or(tag, |e| e.asset.tag.as_str())
            .to_owned();
        let m = package.manifest;
        extensions.push(CatalogEntry {
            id: m.id,
            name: m.name,
            version: m.version,
            description: m.description,
            api: m.api,
            asset: Asset {
                tag,
                name,
                size: bytes.len() as u64,
                sha256,
            },
        });
    }
    let catalog = Catalog {
        format: CATALOG_FORMAT,
        extensions,
    };
    catalog.validate()?;

    let json = serde_json::to_string_pretty(&catalog)? + "\n";
    let signature = BASE64.encode(key.sign(json.as_bytes()).to_bytes()) + "\n";
    let public = public_key(&key)?;
    write(&catalog_path, &json)?;
    write(&repo.join(SIGNATURE_FILE), &signature)?;
    write(&repo.join(KEY_FILE), format!("{}\n", public.to_base64()))?;

    let verified = Catalog::verify(
        &read(&catalog_path)?,
        &read(&repo.join(SIGNATURE_FILE))?,
        &PublicKey::from_base64(&String::from_utf8(read(&repo.join(KEY_FILE))?)?)?,
    )?;
    for e in &verified.extensions {
        println!("{} {} -> {}/{}", e.id, e.version, e.asset.tag, e.asset.name);
    }
    println!("signed by {}", public.fingerprint());
    Ok(())
}

fn check(file: &Path) -> Result {
    let bytes = read(file)?;
    let package = Package::decode(&bytes)?;
    let m = &package.manifest;
    println!("{} {} ({}), API {}", m.id, m.version, m.name, m.api);
    if !m.description.is_empty() {
        println!("  {}", m.description);
    }
    println!(
        "  capabilities: {}",
        serde_json::to_string(&m.capabilities)?
    );
    for s in &m.settings {
        println!(
            "  setting {} \"{}\" = {}",
            s.key,
            s.label,
            s.default_value()
        );
    }
    for c in &m.commands {
        println!("  command {} \"{}\"", c.id, c.label);
    }
    println!(
        "  module {} bytes, package {} bytes, sha256 {}",
        package.module.len(),
        bytes.len(),
        sha256_hex(&bytes)
    );
    Ok(())
}
