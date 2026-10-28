//! `tetrad-ext`: desktop and CI tooling for Tetrad extensions: scaffold, type-check and
//! bundle, package, sign a repository catalog, and estimate and preview DSP blocks.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
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
use tetrad_extensions::package::{AppFile, PACKAGE_SUFFIX, Package};
use tetrad_extensions::sdk::SDK_DECLARATIONS;

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

const MANIFEST_FILE: &str = "tetrad-extension.json";
const ENTRY: &str = "src/index.ts";
const BUNDLE: &str = "dist/index.mjs";
/// The HTML of the extension's Remote tab, packed when the manifest has `remote`.
const REMOTE_PAGE: &str = "remote.html";
/// The program files of a manifest with `app`, each one packed by name.
const APP_DIR: &str = "app";
/// `blocks/<id>.qcdsp.json`: the IR of each static block.
const BLOCKS_DIR: &str = "blocks";
const BLOCK_SUFFIX: &str = ".qcdsp.json";
/// Modeled SHARC+ steps in one 32-sample call on one core (450 MHz at 48 kHz).
const STEPS_PER_CALL: f64 = 300_000.0;
/// qcdsp over-reports generated work by this factor.
const CPU_SAFETY: f64 = 2.0;
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
        /// Also scaffold a static DSP block, `blocks/gain.qcdsp.json`.
        #[arg(long)]
        blocks: bool,
        dir: PathBuf,
    },
    /// Write the SDK declarations (`tetrad.d.ts`).
    Sdk {
        #[arg(long)]
        out: PathBuf,
    },
    /// Type-check with tsc, then bundle `src/index.ts` to `dist/index.mjs` with esbuild (only
    /// when `src/index.ts` exists), and validate `blocks/<id>.qcdsp.json` of every static block.
    Build { dir: PathBuf },
    /// Pack `tetrad-extension.json`, `dist/index.mjs` (when there is a module) and the static
    /// block IR into a `.tetradext` file. A data-only manifest (themes or static blocks, no
    /// capabilities, settings or commands) without `src/index.ts` packs without a module;
    /// themes and blocks are checked here, as the device checks them. With `app` in the
    /// manifest, every file in `app/` is pinned in the package and copied next to it as
    /// `<package stem>.<file>`: publish these files as release assets with the package.
    Package {
        dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write and sign the catalog for `<repo-dir>/packages/*.tetradext`. The program files of a
    /// new package must be next to it, as `tetrad-ext package` writes them.
    Catalog {
        #[arg(long)]
        repo_dir: PathBuf,
        #[arg(long)]
        key: PathBuf,
        /// Release tag of new packages. Packages already in the catalog keep their tag.
        #[arg(long)]
        tag: String,
    },
    /// Validate a package and print its manifest, with a device estimate of each static block.
    Check { file: PathBuf },
    /// Run a static block of a package on a WAV file with qcdsp's host reference. Writes 32-bit
    /// float mono at the input's sample rate.
    Preview {
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        block: String,
        /// 16-bit PCM or 32-bit float WAV; channels are mixed to mono.
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// A parameter in DSP units, `NAME=VALUE`; repeat for more.
        #[arg(long = "set", value_name = "NAME=VALUE")]
        set: Vec<String>,
    },
    /// Development: build an on-device store at `--store` (copy it to
    /// `/media/p4/tetrad/extensions`) with `--source` trusted, its signed catalog cached and
    /// the newest version of every extension in `<repo-dir>` installed, through the same
    /// verified install path the device uses.
    Stage {
        #[arg(long)]
        repo_dir: PathBuf,
        /// `owner/repo` the repository is published as.
        #[arg(long, default_value = tetrad_extensions::repository::DEFAULT_SOURCE)]
        source: String,
        #[arg(long)]
        store: PathBuf,
    },
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Cmd::Keygen { out } => keygen(&out),
        Cmd::Init {
            name,
            id,
            blocks,
            dir,
        } => init(&name, &id, blocks, &dir),
        Cmd::Sdk { out } => write(&out, SDK_DECLARATIONS),
        Cmd::Build { dir } => build(&dir),
        Cmd::Package { dir, out } => package(&dir, &out),
        Cmd::Catalog { repo_dir, key, tag } => catalog(&repo_dir, &key, &tag),
        Cmd::Check { file } => check(&file),
        Cmd::Preview {
            package,
            block,
            input,
            output,
            set,
        } => preview(&package, &block, &input, &output, &set),
        Cmd::Stage {
            repo_dir,
            source,
            store,
        } => stage(&repo_dir, &source, &store),
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

fn init(name: &str, id: &str, blocks: bool, dir: &Path) -> Result {
    let mut manifest: Manifest = serde_json::from_value(serde_json::json!({
        "id": id,
        "name": name,
        "version": "0.1.0",
        "description": "",
        "api": API_VERSION,
        "capabilities": ["storage"],
        "settings": [],
        "commands": [{ "id": "hello", "label": "Say hello" }],
    }))?;
    let gain = blocks.then(qcdsp::examples::gain).transpose()?;
    if gain.is_some() {
        manifest.blocks = vec![serde_json::from_value(serde_json::json!({
            "id": "gain", "name": "Gain", "category": "utility"
        }))?];
    }
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
    if let Some(gain) = gain {
        write(&block_path(dir, "gain"), gain.to_json()? + "\n")?;
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
    let manifest = read_manifest(dir)?;
    if dir.join(ENTRY).exists() {
        run(dir, &["tsc", "-p", ".", "--noEmit"])?;
        run(
            dir,
            &[
                "esbuild",
                ENTRY,
                "--bundle",
                "--format=esm",
                "--platform=neutral",
                "--target=es2020",
                &format!("--outfile={BUNDLE}"),
            ],
        )?;
        println!("built {}", dir.join(BUNDLE).display());
    }
    for id in read_blocks(dir, &manifest)?.keys() {
        println!("checked {}", block_path(dir, id).display());
    }
    Ok(())
}

fn read_manifest(dir: &Path) -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_slice(&read(&dir.join(MANIFEST_FILE))?)
        .map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
    manifest
        .validate()
        .map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
    Ok(manifest)
}

fn block_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(BLOCKS_DIR).join(format!("{id}{BLOCK_SUFFIX}"))
}

/// The validated IR of every static block of `manifest`.
fn read_blocks(dir: &Path, manifest: &Manifest) -> Result<BTreeMap<String, qcdsp::Block>> {
    let mut blocks = BTreeMap::new();
    for spec in manifest.blocks.iter().filter(|b| !b.generated) {
        let path = block_path(dir, &spec.id);
        let block = qcdsp::Block::from_json(&String::from_utf8(read(&path)?)?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        blocks.insert(spec.id.clone(), block);
    }
    Ok(blocks)
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
    let manifest = read_manifest(dir)?;
    let module = if dir.join(BUNDLE).exists() {
        Some(String::from_utf8(read(&dir.join(BUNDLE))?)?)
    } else if dir.join(ENTRY).exists() {
        return Err(format!("{} is missing; run `tetrad-ext build` first", BUNDLE).into());
    } else {
        None
    };
    let blocks = read_blocks(dir, &manifest)?;
    let remote_page = match &manifest.remote {
        Some(_) if dir.join(REMOTE_PAGE).exists() => {
            Some(String::from_utf8(read(&dir.join(REMOTE_PAGE))?)?)
        }
        Some(_) => {
            return Err(format!("the manifest has a remote tab; {REMOTE_PAGE} is missing").into());
        }
        None => None,
    };
    let app_files = match &manifest.app {
        Some(_) => read_app_files(&dir.join(APP_DIR))?,
        None => Vec::new(),
    };
    let bytes = Package {
        manifest,
        module,
        blocks,
        remote_page,
        app_files: app_files.iter().map(|(file, _)| file.clone()).collect(),
    }
    .encode()?;
    write(out, &bytes)?;
    println!("{} ({} bytes)", out.display(), bytes.len());
    let package_name = out
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("bad --out")?;
    for (file, path) in &app_files {
        let asset = out.with_file_name(Package::app_asset_name(package_name, &file.name));
        fs::copy(path, &asset).map_err(|e| format!("{}: {e}", asset.display()))?;
        println!("{} ({} bytes)", asset.display(), file.size);
    }
    Ok(())
}

/// Every regular file in `dir`, in name order, pinned by size and SHA-256.
fn read_app_files(dir: &Path) -> Result<Vec<(AppFile, PathBuf)>> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e} (the manifest has an app)", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    paths.retain(|p| p.is_file());
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let bytes = read(&path)?;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or("bad file name")?;
            let file = AppFile {
                name: name.to_owned(),
                size: bytes.len() as u64,
                sha256: sha256_hex(&bytes),
            };
            Ok((file, path))
        })
        .collect()
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
        let published = previous
            .iter()
            .find(|e| e.asset.name == name && e.asset.sha256 == sha256);
        let tag = published.map_or(tag, |e| e.asset.tag.as_str()).to_owned();
        // The program files of a new package must be here to upload. A published package keeps
        // them in its release, so a checkout without them can still sign the catalog.
        let new_files = (published.is_none()).then_some(&package.app_files);
        for file in new_files.into_iter().flatten() {
            let asset = Package::app_asset_name(&name, &file.name);
            let bytes = read(&repo.join("packages").join(&asset))?;
            if bytes.len() as u64 != file.size || sha256_hex(&bytes) != file.sha256 {
                return Err(format!("{asset} does not match {name}").into());
            }
        }
        let m = package.manifest;
        extensions.push(CatalogEntry {
            app: m.app.is_some(),
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
    for spec in &m.blocks {
        let category = serde_json::to_value(spec.category)?;
        let category = category.as_str().unwrap_or_default();
        print!("  block {} \"{}\" ({category}", spec.id, spec.name);
        if let Some(asset) = &spec.asset {
            print!(
                ", generated from {} {}",
                asset.label,
                asset.extensions.join(" ")
            );
        } else if spec.generated {
            print!(", generated");
        }
        println!(")");
        if let Some(block) = package.blocks.get(&spec.id) {
            println!("    {}", estimate(block)?);
        }
    }
    for t in &m.themes {
        println!("  theme {} \"{}\"", t.id, t.name);
    }
    if let Some(app) = &m.app {
        println!(
            "  program \"{}\": {} {}",
            app.label,
            app.exec,
            app.args.join(" ")
        );
        for f in &package.app_files {
            println!(
                "    file {} ({} bytes, sha256 {})",
                f.name, f.size, f.sha256
            );
        }
    }
    println!(
        "  module {} bytes, package {} bytes, sha256 {}",
        package.module.as_ref().map_or(0, String::len),
        bytes.len(),
        sha256_hex(&bytes)
    );
    Ok(())
}

/// The block's shape and its device cost from qcdsp's code generator. CPU is an estimate.
fn estimate(block: &qcdsp::Block) -> Result<String> {
    let (layout, generated) = qcdsp::device::generate(block)?;
    let cpu = generated.cycles_per_call as f64 / STEPS_PER_CALL * 100.0 * CPU_SAFETY;
    let channels = match block.channels() {
        qcdsp::Channels::Mono => "mono",
        qcdsp::Channels::Stereo => "stereo",
    };
    Ok(format!(
        "IR {}, {channels}, parameters {}; state {} bytes, {} code parcels, {} cycles per call, \
         CPU {cpu:.1}%",
        block.version(),
        block.parameters().len(),
        layout.bytes(),
        generated.parcels.len(),
        generated.cycles_per_call,
    ))
}

fn preview(package_path: &Path, id: &str, input: &Path, output: &Path, set: &[String]) -> Result {
    let package = Package::decode(&read(package_path)?)?;
    let block = match package.manifest.block(id) {
        None => return Err(format!("the package has no block {id:?}").into()),
        Some(spec) if spec.generated => {
            return Err(format!("{id} is generated at run time; the package has no IR").into());
        }
        Some(_) => package.blocks[id].clone(),
    };
    let mut processor = qcdsp::Processor::new(block.clone())?;
    for assignment in set {
        let (name, value) = assignment
            .split_once('=')
            .ok_or_else(|| format!("--set {assignment:?}: use NAME=VALUE"))?;
        let index = block
            .parameters()
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| format!("{id} has no parameter {name:?}"))?;
        let value: f32 = value
            .trim()
            .parse()
            .map_err(|_| format!("--set {assignment:?}: not a number"))?;
        processor.set_parameter(index, value)?;
    }

    let mut reader =
        hound::WavReader::open(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels);
    if channels == 0 {
        return Err(format!("{}: no channels", input.display()).into());
    }
    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()?,
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|s| f32::from(s) / 32768.0))
            .collect::<std::result::Result<_, _>>()?,
        (format, bits) => {
            return Err(format!(
                "{}: {bits}-bit {format:?} is not supported; use 16-bit PCM or 32-bit float",
                input.display()
            )
            .into());
        }
    };
    if spec.sample_rate != 48_000 {
        eprintln!(
            "warning: {} Hz input; the Quad Cortex runs blocks at 48000 Hz",
            spec.sample_rate
        );
    }
    let mut mono: Vec<f32> = samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    processor.process_in_place(&mut mono);

    let mut writer = hound::WavWriter::create(
        output,
        hound::WavSpec {
            channels: 1,
            sample_rate: spec.sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .map_err(|e| format!("{}: {e}", output.display()))?;
    for &sample in &mono {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    // `max` skips NaN, so check finiteness on its own.
    let peak = mono.iter().fold(0f32, |peak, s| peak.max(s.abs()));
    println!(
        "{} ({} samples at {} Hz, peak {peak:.3})",
        output.display(),
        mono.len(),
        spec.sample_rate
    );
    if mono.iter().any(|s| !s.is_finite()) {
        eprintln!("warning: the output has NaN or infinite samples");
    }
    Ok(())
}

fn stage(repo: &Path, source: &str, store_dir: &Path) -> Result {
    use tetrad_extensions::repository::SourceId;
    use tetrad_extensions::store::{Source, Store};

    let source: SourceId = source.parse()?;
    let key_text = String::from_utf8(read(&repo.join(KEY_FILE))?)?;
    let key = PublicKey::from_base64(&key_text)?;
    let json = read(&repo.join(CATALOG_FILE))?;
    let signature = read(&repo.join(SIGNATURE_FILE))?;
    let catalog = Catalog::verify(&json, &signature, &key)?;
    let default = Source::default_source();
    let mut sources = vec![default.clone()];
    if source == default.source {
        if default.key != key {
            return Err("the repository key is not the pinned default key".into());
        }
    } else {
        sources.push(Source {
            source: source.clone(),
            key,
            default: false,
            enabled: true,
        });
    }
    let store = Store::open(store_dir)?;
    store.save_sources(&sources)?;
    store.save_catalog(&source, &json, &signature)?;
    let mut ids: Vec<&str> = catalog.extensions.iter().map(|e| e.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        let Some(entry) = catalog.latest(id) else {
            continue;
        };
        let bytes = read(&repo.join("packages").join(&entry.asset.name))?;
        let files = (Package::decode(&bytes)?.app_files.iter())
            .map(|file| {
                let asset = Package::app_asset_name(&entry.asset.name, &file.name);
                Ok((file.name.clone(), read(&repo.join("packages").join(asset))?))
            })
            .collect::<Result<_>>()?;
        let installed = store.install(&source, &key, entry, &bytes, &files)?;
        println!("installed {} {}", installed.key, installed.current.version);
    }
    Ok(())
}
