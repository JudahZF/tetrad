//! Tetrad self-update: the newest Tetrad release of [`REPOSITORY`], downloaded to p4, checked
//! and armed. CorOS, the root file system and the boot hook do not change. Design:
//! `docs/updates.md` (Tetrad updates).
//!
//! A release that the device can install has the asset [`MANIFEST`]: the version, the
//! [`Channel`] and the size and SHA-256 of each file in [`FILES`]. Each file is also an asset, named by its file name.
//! [`SIGNATURE`] is the Ed25519 signature of the exact manifest bytes by the release key: the
//! release workflow signs with the `TETRAD_RELEASE_KEY` secret, and the device checks it with
//! [`RELEASE_KEY`] before it reads the manifest. So a release asset changed after the build
//! does not install. [`install`] makes the same steps as `scripts/install.sh update`: write each file as
//! `<path>.part`, check its size and SHA-256, check that the scripts parse (`sh -n`) and that
//! the new binary supports the running CorOS (`tetrad run --check`), then move the files into
//! place and arm the binary (`tetrad-launcher.sh arm`). A failure before the move removes the
//! `.part` files and keeps the old Tetrad.
//!
//! [`check`] follows one [`Channel`] (docs/updates.md). Stable releases have the tags
//! `v<semver>`; the nightly release is one pre-release with the tag [`NIGHTLY_TAG`], replaced
//! by each build, with the version in its manifest only. Both are signed and installed the same way; a manifest of the wrong
//! channel is refused. A check never offers an older release as an update: a nightly unit on
//! the stable channel gets the newest stable as [`Found::Older`], to install only on request.
//!
//! A release can also come over USB ([`Source::Usb`], `docs/usb.md`): `tetrad-usb install`
//! pushes the same signed manifest and files, the device writes each file as its `.part` and
//! offers the release on the Tetrad Updates page. [`install`] then runs the same checks; only
//! the download is skipped. Over USB any signed version installs, older ones too, because the
//! user starts the install on the unit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::paths::SCRIPT_DIR;
use crate::exec::{Command, CommandError, CommandRunner, ExitStatus, FileError, Files};
use crate::http::{HttpClient, HttpError, HttpRequest, Method, Route};

/// The GitHub repository of Tetrad releases.
pub const REPOSITORY: &str = "JudahZF/tetrad";
/// The release asset that makes a release installable on the device.
pub const MANIFEST: &str = "tetrad-update.json";
/// The tag of the nightly release (docs/releases.md).
pub const NIGHTLY_TAG: &str = "nightly";
/// The base64 Ed25519 signature of [`MANIFEST`].
pub const SIGNATURE: &str = "tetrad-update.json.sig";
/// The base64 Ed25519 key that signs releases. The release workflow checks that its secret
/// key matches this value. A new key needs one install with `scripts/install.sh`.
pub const RELEASE_KEY: &str = "M4g350Lncizx/83Q9fJVpFZ9oiHvkr/1/2D5fiB8I2o=";
/// The key this build trusts: [`RELEASE_KEY`], or the base64 Ed25519 key in the environment
/// variable `TETRAD_RELEASE_PUBKEY` at build time (docs/development.md, "Your own updates").
/// A build with its own key takes only manifests signed with the matching private key, and not
/// the releases of [`REPOSITORY`]. The release workflow does not set it.
pub const TRUSTED_KEY: &str = match option_env!("TETRAD_RELEASE_PUBKEY") {
    Some(key) => key,
    None => RELEASE_KEY,
};
/// The Tetrad folder on the user data partition.
pub const HOME: &str = "/media/p4/tetrad";
/// The files of an update, relative to [`HOME`], in sorted order. A manifest lists exactly
/// these; the binary is first.
pub const FILES: [&str; 4] = [
    BINARY,
    "launcher/tetrad-launch.sh",
    "launcher/tetrad-launcher.sh",
    "launcher/tetrad-update.sh",
];
const BINARY: &str = "bin/tetrad";
const LAUNCHER: &str = "launcher/tetrad-launcher.sh";

/// Releases read from the GitHub API, newest first. Older releases cannot be newer.
const RELEASES_PER_PAGE: u8 = 30;
const MAX_RELEASES_BYTES: usize = 4 << 20;
const MAX_MANIFEST_BYTES: usize = 64 << 10;
const MAX_SIGNATURE_BYTES: usize = 1 << 10;
/// The largest `bin/tetrad` that an update takes.
pub const MAX_BINARY_BYTES: u64 = 64 << 20;
const MAX_SCRIPT_BYTES: u64 = 256 << 10;
const API_TIMEOUT: Duration = Duration::from_secs(30);
const FILE_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Error)]
pub enum TetradUpdateError {
    #[error("{url}: {error}")]
    Http { url: String, error: HttpError },
    #[error("{url}: HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("{url}: the response is over {max} bytes")]
    TooLarge { url: String, max: usize },
    #[error("the release list is not valid: {0}")]
    Releases(String),
    #[error("{SIGNATURE}: {0}")]
    Signature(&'static str),
    #[error("{MANIFEST} is not valid: {0}")]
    Manifest(String),
    #[error("{0}: the size does not match {MANIFEST}")]
    Size(String),
    #[error("{0}: the SHA-256 does not match {MANIFEST}")]
    Hash(String),
    #[error("{0} does not parse")]
    Script(String),
    #[error("Tetrad {0} does not support the CorOS build on this unit")]
    Unsupported(Version),
    #[error("`{line}` failed: {status:?}")]
    Failed { line: String, status: ExitStatus },
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    File(#[from] FileError),
}

/// Checks the base64 Ed25519 `signature` of `message` against the base64 public `key`: the
/// check of a manifest, and of a key that asks the Remote page of a dev build for its PIN.
pub fn verify_signature(
    key: &str,
    message: &[u8],
    signature: &[u8],
) -> Result<(), TetradUpdateError> {
    let bad = TetradUpdateError::Signature;
    let decode = |text: &[u8]| {
        let text = std::str::from_utf8(text).map_err(|_| bad("not UTF-8"))?;
        BASE64.decode(text.trim()).map_err(|_| bad("not base64"))
    };
    let key: [u8; 32] = (decode(key.as_bytes())?.try_into()).map_err(|_| bad("bad key"))?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| bad("bad key"))?;
    let signature = Signature::from_slice(&decode(signature)?).map_err(|_| bad("not 64 bytes"))?;
    (key.verify_strict(message, &signature)).map_err(|_| bad("does not match the key"))
}

/// Whether `key` is a base64 Ed25519 public key.
pub fn is_public_key(key: &str) -> bool {
    let bytes = BASE64.decode(key.trim()).ok();
    let bytes: Option<[u8; 32]> = bytes.and_then(|bytes| bytes.try_into().ok());
    bytes.is_some_and(|bytes| VerifyingKey::from_bytes(&bytes).is_ok())
}

/// What [`MANIFEST`] says about one file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub size: u64,
    pub sha256: String,
}

/// The release channel that a check follows. (O) Stable: the tested releases. Nightly: the
/// newest build of `main`, not tested.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Nightly,
}

impl Channel {
    pub fn name(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Nightly => "nightly",
        }
    }
}

/// Unknown fields are refused: a new field needs a new Tetrad first. Units up to
/// 0.1.0-alpha.10 also refuse `channel`, so stable manifests leave it out (docs/releases.md).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: Version,
    /// Absent in the manifests from before the channels: stable.
    #[serde(default)]
    pub channel: Channel,
    pub files: BTreeMap<String, FileEntry>,
}

impl Manifest {
    /// Checks `signature` over the exact `json` bytes with the base64 `key`, then parses and
    /// checks the manifest; with `version`, it must be the manifest of that release.
    pub fn verify(
        json: &[u8],
        signature: &[u8],
        key: &str,
        version: Option<&Version>,
    ) -> Result<Self, TetradUpdateError> {
        verify_signature(key, json, signature)?;
        Self::parse(json, version)
    }

    fn parse(json: &[u8], version: Option<&Version>) -> Result<Self, TetradUpdateError> {
        let bad = |text: String| TetradUpdateError::Manifest(text);
        let manifest: Self = serde_json::from_slice(json).map_err(|e| bad(e.to_string()))?;
        if let Some(version) = version
            && &manifest.version != version
        {
            return Err(bad(format!("version {}, not {version}", manifest.version)));
        }
        if !manifest.files.keys().map(String::as_str).eq(FILES) {
            return Err(bad(format!("the files are not {}", FILES.join(", "))));
        }
        for (path, entry) in &manifest.files {
            let max = if path == BINARY {
                MAX_BINARY_BYTES
            } else {
                MAX_SCRIPT_BYTES
            };
            if entry.size == 0 || entry.size > max {
                return Err(bad(format!("{path}: size {} is not 1..={max}", entry.size)));
            }
            let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
            if entry.sha256.len() != 64 || !entry.sha256.bytes().all(hex) {
                return Err(bad(format!(
                    "{path}: sha256 is not 64 lowercase hex digits"
                )));
            }
        }
        Ok(manifest)
    }
}

/// The largest size of update file `path`, or `None` when it is not one of [`FILES`].
pub fn max_bytes(path: &str) -> Option<u64> {
    match path {
        BINARY => Some(MAX_BINARY_BYTES),
        _ if FILES.contains(&path) => Some(MAX_SCRIPT_BYTES),
        _ => None,
    }
}

/// The manifest of an update of some of the [`FILES`], which the unit makes from the files that
/// the Remote page of a dev build staged (docs/remote.md). Nobody signs it, and the user starts
/// the install on the unit. The version is `running` with the build metadata `+http`: the
/// version of a new binary is not known before it runs, and the unit does not run it before the
/// install.
pub fn staged_manifest(running: &Version, files: BTreeMap<String, FileEntry>) -> Manifest {
    let mut version = running.clone();
    version.build = semver::BuildMetadata::new("http").unwrap_or_default();
    Manifest {
        version,
        channel: Channel::default(),
        files,
    }
}

/// Where the files of an [`Offer`] come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The assets of this GitHub release tag.
    Release(String),
    /// Already on p4 as `.part` files ([`part`]), pushed over USB.
    Usb,
}

/// A release that the device can install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub source: Source,
    pub manifest: Manifest,
}

impl Offer {
    pub fn version(&self) -> &Version {
        &self.manifest.version
    }
}

/// The release asset name of a manifest path: its file name.
pub fn asset_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn asset_url(tag: &str, name: &str) -> String {
    format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{name}")
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
}

impl Release {
    fn installable(&self) -> bool {
        !self.draft && self.assets.iter().any(|asset| asset.name == MANIFEST)
    }
}

/// Whether `version` is a nightly build: its first pre-release identifier is `nightly`
/// (`0.1.0-nightly.20261112.317+61697fa`, docs/releases.md).
pub fn is_nightly(version: &Version) -> bool {
    version.pre.split('.').next() == Some("nightly")
}

/// `a` is newer than `b` by semver precedence: build metadata (`+abc1234`) does not count.
pub fn newer(a: &Version, b: &Version) -> bool {
    a.cmp_precedence(b).is_gt()
}

fn releases(json: &[u8]) -> Result<Vec<Release>, TetradUpdateError> {
    serde_json::from_slice(json).map_err(|e| TetradUpdateError::Releases(e.to_string()))
}

/// The newest stable release (tag `v<semver>`, with [`MANIFEST`]), newer than `current` or
/// not. A pre-release (`-alpha.N`, `-rc.N`) counts only when `current` is a pre-release too, so
/// a Tetrad on a plain version stays on plain versions. A nightly version never counts.
fn newest_stable(releases: &[Release], current: &Version) -> Option<(String, Version)> {
    (releases.iter())
        .filter(|release| release.installable())
        .filter_map(|release| {
            let version = (release.tag_name.strip_prefix('v')?)
                .parse::<Version>()
                .ok()?;
            let allowed = version.pre.is_empty() || !current.pre.is_empty();
            (allowed && !is_nightly(&version)).then(|| (release.tag_name.clone(), version))
        })
        .max_by(|a, b| a.1.cmp_precedence(&b.1))
}

/// Whether the list has the nightly release, with [`MANIFEST`].
fn has_nightly(releases: &[Release]) -> bool {
    (releases.iter()).any(|release| release.installable() && release.tag_name == NIGHTLY_TAG)
}

fn get(http: &mut impl HttpClient, url: String, max: usize) -> Result<Vec<u8>, TetradUpdateError> {
    let request = HttpRequest::new(Method::Get, url.as_str(), Route::Product, API_TIMEOUT)
        .header("User-Agent", "Tetrad")
        .header("Accept", "application/vnd.github+json")
        .follow_redirects();
    match http.send(&request) {
        Err(error) => Err(TetradUpdateError::Http { url, error }),
        Ok(response) if !response.is_success() => Err(TetradUpdateError::Status {
            url,
            status: response.status,
        }),
        Ok(response) if response.body.len() > max => Err(TetradUpdateError::TooLarge { url, max }),
        Ok(response) => Ok(response.body),
    }
}

/// What [`check`] found on a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    UpToDate,
    /// A release that is newer than the running one.
    Newer(Offer),
    /// Stable channel, a nightly runs: the newest stable release, which is older than the
    /// running nightly. (O) It installs only when the user asks; else the unit waits for a
    /// stable release that is newer.
    Older(Offer),
}

/// The checked manifest of release `tag`: the signature by `key`, then `version` (the tag's
/// version, when the tag has one) and `channel`.
fn offer(
    http: &mut impl HttpClient,
    key: &str,
    tag: &str,
    version: Option<&Version>,
    channel: Channel,
) -> Result<Offer, TetradUpdateError> {
    let json = get(http, asset_url(tag, MANIFEST), MAX_MANIFEST_BYTES)?;
    let signature = get(http, asset_url(tag, SIGNATURE), MAX_SIGNATURE_BYTES)?;
    let manifest = Manifest::verify(&json, &signature, key, version)?;
    if manifest.channel != channel {
        return Err(TetradUpdateError::Manifest(format!(
            "release {tag} is on the {} channel, not {}",
            manifest.channel.name(),
            channel.name()
        )));
    }
    // A nightly that does not say so is never seen as one after its install, so the way back
    // to stable (`Found::Older`) would not show.
    if channel == Channel::Nightly && !is_nightly(&manifest.version) {
        return Err(TetradUpdateError::Manifest(format!(
            "version {} is not a nightly version",
            manifest.version
        )));
    }
    Ok(Offer {
        source: Source::Release(tag.to_owned()),
        manifest,
    })
}

/// The newest release on `channel` for a unit that runs `current`, with its checked manifest.
/// The nightly channel takes a stable release too when it is newer than the nightly.
/// `key` signs the manifests: [`TRUSTED_KEY`] on a unit.
pub fn check(
    http: &mut impl HttpClient,
    key: &str,
    current: &Version,
    channel: Channel,
) -> Result<Found, TetradUpdateError> {
    let url =
        format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page={RELEASES_PER_PAGE}");
    let releases = releases(&get(http, url, MAX_RELEASES_BYTES)?)?;
    let stable = newest_stable(&releases, current);
    let newer_stable = stable
        .clone()
        .filter(|(_, version)| newer(version, current));
    let nightly = match channel {
        Channel::Stable => None,
        Channel::Nightly if has_nightly(&releases) => {
            Some(offer(http, key, NIGHTLY_TAG, None, Channel::Nightly)?)
        }
        Channel::Nightly => None,
    };
    let nightly = nightly.filter(|nightly| newer(nightly.version(), current));
    Ok(match (nightly, newer_stable, stable) {
        (Some(nightly), Some((tag, version)), _) if newer(&version, nightly.version()) => {
            Found::Newer(offer(http, key, &tag, Some(&version), Channel::Stable)?)
        }
        (Some(nightly), _, _) => Found::Newer(nightly),
        (None, Some((tag, version)), _) => {
            Found::Newer(offer(http, key, &tag, Some(&version), Channel::Stable)?)
        }
        (None, None, Some((tag, version))) if channel == Channel::Stable && is_nightly(current) => {
            Found::Older(offer(http, key, &tag, Some(&version), Channel::Stable)?)
        }
        (None, None, _) => Found::UpToDate,
    })
}

fn home(path: &str) -> PathBuf {
    Path::new(HOME).join(path)
}

/// Where a file of an update is staged before the install moves it into place.
pub fn part(path: &str) -> PathBuf {
    home(&format!("{path}.part"))
}

/// The share of the binary download that is on p4, `0.0..=1.0`, while [`install`] runs.
pub fn progress(files: &mut impl Files, offer: &Offer) -> Result<Option<f32>, FileError> {
    let Some(entry) = offer.manifest.files.get(BINARY) else {
        return Ok(None);
    };
    let have = files.size(&part(BINARY))?;
    Ok(have.map(|have| (have as f32 / entry.size as f32).min(1.0)))
}

fn run(runner: &mut impl CommandRunner, command: &Command) -> Result<(), TetradUpdateError> {
    let output = runner.run(command)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(TetradUpdateError::Failed {
            line: command.line(),
            status: output.status,
        })
    }
}

/// Downloads (unless it came over USB), checks, installs and arms `offer`. The caller restarts
/// the unit after `Ok`.
pub fn install(
    http: &mut impl HttpClient,
    files: &mut impl Files,
    runner: &mut impl CommandRunner,
    offer: &Offer,
) -> Result<(), TetradUpdateError> {
    for dir in ["bin", "launcher"] {
        files.create_dirs(&home(dir))?;
    }
    let staged = stage(http, files, runner, offer);
    if staged.is_err() {
        for path in offer.manifest.files.keys() {
            let _ = files.remove(&part(path));
        }
        return staged;
    }
    // The commit: from here the new files are in place. A failed arm leaves `select` with the
    // old md5, so the launcher starts stock CorOS until `tetrad-launcher.sh arm` runs. The
    // downloads are synced first, so a power cut never leaves a renamed file half written.
    files.sync()?;
    for path in offer.manifest.files.keys() {
        files.rename(&part(path), &home(path))?;
    }
    files.sync()?;
    let launcher = home(LAUNCHER).display().to_string();
    run(runner, &Command::new("sh", [launcher.as_str(), "arm"]))
}

/// Every file as `.part`, checked. Nothing in place changes.
fn stage(
    http: &mut impl HttpClient,
    files: &mut impl Files,
    runner: &mut impl CommandRunner,
    offer: &Offer,
) -> Result<(), TetradUpdateError> {
    for (path, entry) in &offer.manifest.files {
        let dest = part(path);
        if let Source::Release(tag) = &offer.source {
            let url = asset_url(tag, asset_name(path));
            let request = HttpRequest::new(Method::Get, url.as_str(), Route::Product, FILE_TIMEOUT)
                .header("User-Agent", "Tetrad")
                .follow_redirects();
            let status =
                (http.download(&request, &dest)).map_err(|error| TetradUpdateError::Http {
                    url: url.clone(),
                    error,
                })?;
            if !(200..300).contains(&status) {
                return Err(TetradUpdateError::Status { url, status });
            }
        }
        if files.size(&dest)? != Some(entry.size) {
            return Err(TetradUpdateError::Size(path.clone()));
        }
        let bytes = files.read(&dest)?.unwrap_or_default();
        let sha256: String = (Sha256::digest(&bytes).iter())
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if sha256 != entry.sha256 {
            return Err(TetradUpdateError::Hash(path.clone()));
        }
    }
    // The files of the offer: all four of a release, or the binary alone ([`binary_manifest`]).
    let parts: Vec<(&String, String)> = (offer.manifest.files.keys())
        .map(|path| (path, part(path).display().to_string()))
        .collect();
    run(
        runner,
        &Command::new(
            "chmod",
            ["755"]
                .into_iter()
                .chain(parts.iter().map(|(_, file)| file.as_str())),
        ),
    )?;
    for (path, file) in parts.iter().filter(|(path, _)| *path != BINARY) {
        if !runner
            .run(&Command::new("sh", ["-n", file.as_str()]))?
            .status
            .success()
        {
            return Err(TetradUpdateError::Script((*path).clone()));
        }
    }
    // Scripts alone: the installed binary is checked, as the scripts will start it.
    let binary = if offer.manifest.files.contains_key(BINARY) {
        part(BINARY)
    } else {
        home(BINARY)
    };
    let check = Command::new(&binary.display().to_string(), ["run", "--check"]).cwd(SCRIPT_DIR);
    match runner.run(&check)?.status {
        status if status.success() => Ok(()),
        ExitStatus::Code(4) => Err(TetradUpdateError::Unsupported(offer.version().clone())),
        status => Err(TetradUpdateError::Failed {
            line: check.line(),
            status,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Output;
    use crate::fake::{MemFiles, ScriptedHttp, ScriptedRunner};

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    fn release(tag: &str, with_manifest: bool) -> serde_json::Value {
        let assets = if with_manifest {
            serde_json::json!([{ "name": "tetrad" }, { "name": MANIFEST }])
        } else {
            serde_json::json!([{ "name": "tetrad" }])
        };
        serde_json::json!({ "tag_name": tag, "draft": false, "body": format!("notes {tag}"), "assets": assets })
    }

    /// The newest stable release with a manifest wins; releases without a manifest, drafts and
    /// nightlies do not count. A Tetrad on a plain version never takes a pre-release.
    #[test]
    fn stable_lookup_follows_semver_and_prereleases() {
        let mut draft = release("v0.3.0", true);
        draft["draft"] = true.into();
        let list = serde_json::json!([
            draft,
            release("v0.2.0", false),
            release("v0.1.0", true),
            release("v0.2.0-alpha.1", true),
            release("v0.1.0-alpha.3", true),
            release("v0.3.0-nightly.20261112", true),
            release("nightly", true),
        ])
        .to_string();
        let releases = releases(list.as_bytes()).unwrap();
        let pick = |current| newest_stable(&releases, &v(current)).map(|r| r.0);
        assert_eq!(pick("0.1.0-alpha.3").as_deref(), Some("v0.2.0-alpha.1"));
        assert_eq!(pick("0.0.9").as_deref(), Some("v0.1.0"));
        // The newest stable even when it is not newer: `check` decides what to offer.
        assert_eq!(pick("0.2.0-alpha.1").as_deref(), Some("v0.2.0-alpha.1"));
        assert_eq!(
            pick("0.3.0-nightly.20261113").as_deref(),
            Some("v0.2.0-alpha.1")
        );
        assert!(has_nightly(&releases));
        let others =
            serde_json::json!([release("nightly", false), release("nightly-20261112", true)]);
        assert!(!has_nightly(
            &super::releases(others.to_string().as_bytes()).unwrap()
        ));
    }

    /// A nightly sorts after the stable it follows and before the next one; build metadata
    /// does not make a build newer.
    #[test]
    fn nightly_versions_sort_between_stables() {
        // Nightlies of a pre-release (`0.1.0-alpha.10`) take its core; of a release, the next
        // patch. Two builds of one day sort by their UTC time (`.317` is 03:17).
        let order = [
            "0.1.0-alpha.10",
            "0.1.0-nightly.20261112.317+61697fa",
            "0.1.0-nightly.20261112.1305+0123abc",
            "0.1.0-nightly.20261113.317+89abcde",
            "0.1.0",
            "0.1.1-nightly.20261114.317+abc1234",
            "0.1.1",
        ];
        for pair in order.windows(2) {
            assert!(newer(&v(pair[1]), &v(pair[0])), "{} > {}", pair[1], pair[0]);
            assert!(
                !newer(&v(pair[0]), &v(pair[1])),
                "{} < {}",
                pair[0],
                pair[1]
            );
        }
        let (a, b) = (
            v("0.1.1-nightly.20261112+abc"),
            v("0.1.1-nightly.20261112+def"),
        );
        assert!(!newer(&a, &b) && !newer(&b, &a));
        assert!(is_nightly(&a));
        for stable in [
            "0.1.0-alpha.10.nightly.20261112",
            "0.1.0",
            "0.1.0-alpha.10",
            "0.1.0+nightly",
            "0.1.0-nightlyx.1",
        ] {
            assert!(!is_nightly(&v(stable)), "{stable}");
        }
    }

    /// A signed manifest of `version` on `channel` (`None`: no `channel` field, as before the
    /// channels), with the key that checks it.
    fn signed(version: &str, channel: Option<&str>) -> (String, String, String) {
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(&[7; 32]);
        let mut json =
            serde_json::json!({ "version": version, "files": files_json(b"elf", b"sh") });
        if let Some(channel) = channel {
            json["channel"] = channel.into();
        }
        let json = json.to_string();
        let signature = BASE64.encode(key.sign(json.as_bytes()).to_bytes());
        (
            BASE64.encode(key.verifying_key().to_bytes()),
            json,
            signature,
        )
    }

    /// Runs [`check`] on `list` with the manifests that follow it served in order.
    fn checked(
        list: &serde_json::Value,
        manifests: &[(&str, Option<&str>)],
        current: &str,
        channel: Channel,
    ) -> (Result<Found, TetradUpdateError>, Vec<String>) {
        let mut http = ScriptedHttp::default();
        http.respond(200, list.to_string());
        for &(version, manifest_channel) in manifests {
            let (_, json, signature) = signed(version, manifest_channel);
            http.respond(200, json).respond(200, signature);
        }
        let key = signed("0.0.0", None).0;
        let found = check(&mut http, &key, &v(current), channel);
        let urls = (http.requests.iter()).map(|r| r.url.clone()).collect();
        (found, urls)
    }

    fn version_of(found: &Found) -> (&'static str, String) {
        match found {
            Found::UpToDate => ("up to date", String::new()),
            Found::Newer(offer) => ("newer", offer.version().to_string()),
            Found::Older(offer) => ("older", offer.version().to_string()),
        }
    }

    /// The stable channel never reads a nightly; the nightly channel takes the newest of the
    /// newest nightly and the stable releases. Nothing older is offered as an update.
    #[test]
    fn check_follows_the_channel() {
        let list = serde_json::json!([release("nightly", true), release("v0.1.0-alpha.11", true)]);
        let (found, urls) = checked(
            &list,
            &[("0.1.0-alpha.11", None)],
            "0.1.0-alpha.10",
            Channel::Stable,
        );
        assert_eq!(
            version_of(&found.unwrap()),
            ("newer", "0.1.0-alpha.11".into())
        );
        assert!(
            urls.iter().all(|url| !url.contains("/nightly/")),
            "{urls:?}"
        );

        let nightly = [("0.1.1-nightly.20261112+abc1234", Some("nightly"))];
        let (found, urls) = checked(&list, &nightly, "0.1.0-alpha.10", Channel::Nightly);
        assert_eq!(
            version_of(&found.unwrap()),
            ("newer", "0.1.1-nightly.20261112+abc1234".into())
        );
        assert_eq!(urls.len(), 3, "{urls:?}");
        assert!(urls[1].ends_with("/download/nightly/tetrad-update.json"));

        // The running nightly is the newest build.
        let (found, _) = checked(
            &list,
            &nightly,
            "0.1.1-nightly.20261112+abc1234",
            Channel::Nightly,
        );
        assert_eq!(found.unwrap(), Found::UpToDate);

        // A stable release newer than the newest nightly wins on the nightly channel.
        let list = serde_json::json!([release("nightly", true), release("v0.1.1", true)]);
        let both = [nightly[0], ("0.1.1", None)];
        let (found, _) = checked(&list, &both, "0.1.0", Channel::Nightly);
        assert_eq!(version_of(&found.unwrap()), ("newer", "0.1.1".into()));
    }

    /// Back to stable from a nightly: the older stable is found as [`Found::Older`], never as an
    /// update. A stable unit with a newer version than every release is just up to date.
    #[test]
    fn a_nightly_on_the_stable_channel_gets_the_older_stable_on_request_only() {
        let list = serde_json::json!([release("nightly", true), release("v0.1.0", true)]);
        let stable = [("0.1.0", None)];
        let (found, _) = checked(
            &list,
            &stable,
            "0.1.1-nightly.20261112+abc1234",
            Channel::Stable,
        );
        assert_eq!(version_of(&found.unwrap()), ("older", "0.1.0".into()));
        // Part of the scheme: every 0.1.0 nightly sorts after 0.1.0-alpha.N.
        let list = serde_json::json!([release("nightly", true), release("v0.1.0-alpha.11", true)]);
        let alpha = [("0.1.0-alpha.11", None)];
        let current = "0.1.0-nightly.20261112.317+61697fa";
        let (found, _) = checked(&list, &alpha, current, Channel::Stable);
        assert_eq!(
            version_of(&found.unwrap()),
            ("older", "0.1.0-alpha.11".into())
        );
        let (found, urls) = checked(&list, &[], "0.1.1-rc.1", Channel::Stable);
        assert_eq!(found.unwrap(), Found::UpToDate);
        assert_eq!(urls.len(), 1);
        // A stable newer than the nightly is a normal update.
        let list = serde_json::json!([release("v0.1.1", true)]);
        let (found, _) = checked(
            &list,
            &[("0.1.1", None)],
            "0.1.1-nightly.20261112",
            Channel::Stable,
        );
        assert_eq!(version_of(&found.unwrap()), ("newer", "0.1.1".into()));
    }

    /// A manifest must be of the channel the check asked for, after a good signature: a
    /// nightly tag with a stable manifest, or a stable tag with a nightly one, is refused.
    #[test]
    fn check_refuses_a_manifest_of_the_other_channel() {
        let list = serde_json::json!([release("nightly", true)]);
        let (found, _) = checked(
            &list,
            &[("0.1.1-nightly.1", None)],
            "0.1.0",
            Channel::Nightly,
        );
        assert!(
            matches!(found, Err(TetradUpdateError::Manifest(e)) if e.contains("stable channel"))
        );
        let (found, _) = checked(
            &list,
            &[("0.1.1", Some("nightly"))],
            "0.1.0",
            Channel::Nightly,
        );
        assert!(
            matches!(found, Err(TetradUpdateError::Manifest(e)) if e.contains("not a nightly"))
        );
        let list = serde_json::json!([release("v0.1.1", true)]);
        let (found, _) = checked(
            &list,
            &[("0.1.1", Some("nightly"))],
            "0.1.0",
            Channel::Stable,
        );
        assert!(
            matches!(found, Err(TetradUpdateError::Manifest(e)) if e.contains("nightly channel"))
        );
    }

    /// `channel` is optional (stable) and has two values; any other value is refused.
    #[test]
    fn manifest_channel_defaults_to_stable() {
        let files = files_json(b"elf", b"sh");
        let parse = |channel: Option<&str>| {
            let mut json = serde_json::json!({ "version": "0.2.0", "files": files });
            if let Some(channel) = channel {
                json["channel"] = channel.into();
            }
            Manifest::parse(json.to_string().as_bytes(), None).map(|m| m.channel)
        };
        assert_eq!(parse(None).unwrap(), Channel::Stable);
        assert_eq!(parse(Some("stable")).unwrap(), Channel::Stable);
        assert_eq!(parse(Some("nightly")).unwrap(), Channel::Nightly);
        assert!(parse(Some("beta")).is_err());
    }

    fn files_json(binary: &[u8], script: &[u8]) -> serde_json::Value {
        let entry = |bytes: &[u8]| {
            let sha256: String = Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            serde_json::json!({ "size": bytes.len(), "sha256": sha256 })
        };
        serde_json::json!({
            "bin/tetrad": entry(binary),
            "launcher/tetrad-launch.sh": entry(script),
            "launcher/tetrad-launcher.sh": entry(script),
            "launcher/tetrad-update.sh": entry(script),
        })
    }

    /// A signature that `openssl pkeyutl -sign -rawin` made (as the release workflow does)
    /// verifies; a changed manifest, another key or a bad encoding does not. The fixture key
    /// was thrown away after it signed.
    #[test]
    fn manifest_signature_matches_openssl() {
        const KEY: &str = "Ju0bZNj0CZi5eGkhrfTVlboFRqfIyg0qcvOaFKkT/4A=";
        const SIG: &[u8] = b"ZVpvi73G+v7iydeEufUWZxrA+kojyoEI3ekuU0MnBNFtuO8DjaAv8fwGYK+mMUaBl62YfakhhJCCkSlM/qSpAA==\n";
        let json = br#"{"version":"1.2.3"}"#;
        let signed = |json: &[u8], key| Manifest::verify(json, SIG, key, Some(&v("1.2.3")));
        // The signature holds; the fixture has no files, so the manifest check fails next.
        assert!(matches!(
            signed(json, KEY),
            Err(TetradUpdateError::Manifest(_))
        ));
        let changed = br#"{"version":"1.2.4"}"#;
        assert!(matches!(
            signed(changed, KEY),
            Err(TetradUpdateError::Signature(_))
        ));
        assert!(matches!(
            signed(json, RELEASE_KEY),
            Err(TetradUpdateError::Signature(_))
        ));
        let garbage = Manifest::verify(json, b"%%", KEY, None);
        assert!(matches!(garbage, Err(TetradUpdateError::Signature(_))));
    }

    /// The manifest must name exactly the update files, for the release's own version.
    #[test]
    fn manifest_rejects_other_files_and_versions() {
        let files = files_json(b"elf", b"sh");
        let json = |version: &str, files: &serde_json::Value| {
            serde_json::json!({ "version": version, "files": files }).to_string()
        };
        assert!(Manifest::parse(json("0.2.0", &files).as_bytes(), Some(&v("0.2.0"))).is_ok());
        assert!(Manifest::parse(json("0.2.1", &files).as_bytes(), Some(&v("0.2.0"))).is_err());
        // Over USB the version is free: the user starts the install on the unit.
        assert!(Manifest::parse(json("0.1.0", &files).as_bytes(), None).is_ok());
        let mut extra = files.clone();
        extra["../../etc/init.d/S80drvinit"] = files["bin/tetrad"].clone();
        assert!(Manifest::parse(json("0.2.0", &extra).as_bytes(), Some(&v("0.2.0"))).is_err());
        let mut missing = files.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("launcher/tetrad-update.sh");
        assert!(Manifest::parse(json("0.2.0", &missing).as_bytes(), Some(&v("0.2.0"))).is_err());
    }

    fn offer(binary: &[u8], script: &[u8]) -> Offer {
        let json = serde_json::json!({ "version": "0.2.0", "files": files_json(binary, script) });
        Offer {
            source: Source::Release("v0.2.0".into()),
            manifest: Manifest::parse(json.to_string().as_bytes(), Some(&v("0.2.0"))).unwrap(),
        }
    }

    const PARTS: &str = "/media/p4/tetrad/bin/tetrad.part /media/p4/tetrad/launcher/tetrad-launch.sh.part /media/p4/tetrad/launcher/tetrad-launcher.sh.part /media/p4/tetrad/launcher/tetrad-update.sh.part";

    fn checks(runner: &mut ScriptedRunner, run_check: i32) {
        runner.expect_ok(&format!("chmod 755 {PARTS}"));
        for name in ["tetrad-launch.sh", "tetrad-launcher.sh", "tetrad-update.sh"] {
            runner.expect_ok(&format!("sh -n /media/p4/tetrad/launcher/{name}.part"));
        }
        runner.expect(
            "/media/p4/tetrad/bin/tetrad.part run --check",
            Ok(Output::code(run_check)),
        );
    }

    fn served(files: &MemFiles, bodies: [&[u8]; 4]) -> ScriptedHttp {
        let mut http = ScriptedHttp::sharing(files.clone());
        for body in bodies {
            http.respond(200, body);
        }
        http
    }

    /// A USB offer installs the `.part` files already on p4 with the same checks, and never
    /// touches the network.
    #[test]
    fn a_usb_offer_installs_the_pushed_parts() {
        let files = MemFiles::default();
        files.put("/media/p4/tetrad/bin/tetrad.part", "new-elf");
        for name in ["tetrad-launch.sh", "tetrad-launcher.sh", "tetrad-update.sh"] {
            files.put(&format!("/media/p4/tetrad/launcher/{name}.part"), "sh");
        }
        let mut http = ScriptedHttp::sharing(files.clone());
        let mut runner = ScriptedRunner::default();
        checks(&mut runner, 0);
        runner.expect_ok("sh /media/p4/tetrad/launcher/tetrad-launcher.sh arm");
        let offer = Offer {
            source: Source::Usb,
            ..offer(b"new-elf", b"sh")
        };
        install(&mut http, &mut files.clone(), &mut runner, &offer).unwrap();
        assert!(runner.is_done());
        assert!(http.requests.is_empty());
        assert_eq!(
            files.get("/media/p4/tetrad/bin/tetrad").as_deref(),
            Some(&b"new-elf"[..])
        );
    }

    fn entry(bytes: &[u8]) -> FileEntry {
        let sha256: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        FileEntry {
            size: bytes.len() as u64,
            sha256,
        }
    }

    /// A staged offer (the Remote page of a dev build) moves only the files it lists, checks
    /// them, arms, and leaves the other files alone.
    #[test]
    fn a_staged_offer_installs_just_its_files() {
        let files = MemFiles::default();
        files.put("/media/p4/tetrad/bin/tetrad", "old");
        files.put("/media/p4/tetrad/launcher/tetrad-launcher.sh", "launcher");
        files.put("/media/p4/tetrad/bin/tetrad.part", "new-elf");
        let manifest = staged_manifest(
            &v("0.1.0-alpha.8"),
            BTreeMap::from([(BINARY.to_owned(), entry(b"new-elf"))]),
        );
        assert_eq!(manifest.version.to_string(), "0.1.0-alpha.8+http");
        let offer = Offer {
            source: Source::Usb,
            manifest,
        };
        let mut http = ScriptedHttp::sharing(files.clone());
        let mut runner = ScriptedRunner::default();
        runner.expect_ok("chmod 755 /media/p4/tetrad/bin/tetrad.part");
        runner.expect(
            "/media/p4/tetrad/bin/tetrad.part run --check",
            Ok(Output::code(0)),
        );
        runner.expect_ok("sh /media/p4/tetrad/launcher/tetrad-launcher.sh arm");
        install(&mut http, &mut files.clone(), &mut runner, &offer).unwrap();
        assert!(runner.is_done());
        assert!(http.requests.is_empty());
        assert_eq!(
            files.get("/media/p4/tetrad/bin/tetrad").as_deref(),
            Some(&b"new-elf"[..])
        );
        assert_eq!(
            files
                .get("/media/p4/tetrad/launcher/tetrad-launcher.sh")
                .as_deref(),
            Some(&b"launcher"[..])
        );
        assert_eq!(files.get("/media/p4/tetrad/bin/tetrad.part"), None);
    }

    /// Scripts alone: each must parse, the installed binary is checked, only the scripts move.
    #[test]
    fn a_scripts_only_offer_checks_the_installed_binary() {
        let files = MemFiles::default();
        files.put("/media/p4/tetrad/bin/tetrad", "installed");
        let launcher = "/media/p4/tetrad/launcher/tetrad-launcher.sh";
        files.put(&format!("{launcher}.part"), "new launcher");
        let manifest = staged_manifest(
            &v("0.1.0"),
            BTreeMap::from([(
                "launcher/tetrad-launcher.sh".to_owned(),
                entry(b"new launcher"),
            )]),
        );
        let offer = Offer {
            source: Source::Usb,
            manifest,
        };
        let mut runner = ScriptedRunner::default();
        runner.expect_ok(&format!("chmod 755 {launcher}.part"));
        runner.expect_ok(&format!("sh -n {launcher}.part"));
        runner.expect(
            "/media/p4/tetrad/bin/tetrad run --check",
            Ok(Output::code(0)),
        );
        runner.expect_ok(&format!("sh {launcher} arm"));
        let mut http = ScriptedHttp::sharing(files.clone());
        install(&mut http, &mut files.clone(), &mut runner, &offer).unwrap();
        assert!(runner.is_done());
        assert_eq!(files.get(launcher).as_deref(), Some(&b"new launcher"[..]));
        assert_eq!(
            files.get("/media/p4/tetrad/bin/tetrad").as_deref(),
            Some(&b"installed"[..])
        );
    }

    /// Only the four update files have a size limit.
    #[test]
    fn only_update_files_have_a_limit() {
        assert_eq!(max_bytes("bin/tetrad"), Some(MAX_BINARY_BYTES));
        assert_eq!(
            max_bytes("launcher/tetrad-update.sh"),
            Some(MAX_SCRIPT_BYTES)
        );
        for path in ["launcher/select", "../etc/passwd", "bin/tetrad.part", ""] {
            assert_eq!(max_bytes(path), None, "{path}");
        }
    }

    /// A wrong hash or an unsupported CorOS build removes the `.part` files and keeps the old
    /// Tetrad; nothing is armed.
    #[test]
    fn a_failed_check_keeps_the_old_tetrad() {
        let files = MemFiles::default();
        files.put("/media/p4/tetrad/bin/tetrad", "old");
        let mut http = served(&files, [b"new-elg", b"sh", b"sh", b"sh"]);
        let error = install(
            &mut http,
            &mut files.clone(),
            &mut ScriptedRunner::default(),
            &offer(b"new-elf", b"sh"),
        );
        assert!(
            matches!(error, Err(TetradUpdateError::Hash(_))),
            "{error:?}"
        );

        let mut http = served(&files, [b"new-elf", b"sh", b"sh", b"sh"]);
        let mut runner = ScriptedRunner::default();
        checks(&mut runner, 4);
        let error = install(
            &mut http,
            &mut files.clone(),
            &mut runner,
            &offer(b"new-elf", b"sh"),
        );
        assert!(
            matches!(error, Err(TetradUpdateError::Unsupported(_))),
            "{error:?}"
        );
        assert!(runner.is_done());
        assert_eq!(
            files.get("/media/p4/tetrad/bin/tetrad").as_deref(),
            Some(&b"old"[..])
        );
        for path in FILES {
            assert_eq!(files.get(&part(path).display().to_string()), None, "{path}");
        }
    }
}
