//! Tetrad self-update: the newest Tetrad release of [`REPOSITORY`], downloaded to p4, checked
//! and armed. CorOS, the root file system and the boot hook do not change. Design:
//! `docs/updates.md` (Tetrad updates).
//!
//! A release that the device can install has the asset [`MANIFEST`]: the version and the size
//! and SHA-256 of each file in [`FILES`]. Each file is also an asset, named by its file name.
//! [`SIGNATURE`] is the Ed25519 signature of the exact manifest bytes by the release key: the
//! release workflow signs with the `TETRAD_RELEASE_KEY` secret, and the device checks it with
//! [`RELEASE_KEY`] before it reads the manifest. So a release asset changed after the build
//! does not install. [`install`] makes the same steps as `scripts/install.sh update`: write each file as
//! `<path>.part`, check its size and SHA-256, check that the scripts parse (`sh -n`) and that
//! the new binary supports the running CorOS (`tetrad run --check`), then move the files into
//! place and arm the binary (`tetrad-launcher.sh arm`). A failure before the move removes the
//! `.part` files and keeps the old Tetrad.
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
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::paths::SCRIPT_DIR;
use crate::exec::{Command, CommandError, CommandRunner, ExitStatus, FileError, Files};
use crate::http::{HttpClient, HttpError, HttpRequest, Method, Route};

/// The GitHub repository of Tetrad releases.
pub const REPOSITORY: &str = "JudahZF/tetrad";
/// The release asset that makes a release installable on the device.
pub const MANIFEST: &str = "tetrad-update.json";
/// The base64 Ed25519 signature of [`MANIFEST`].
pub const SIGNATURE: &str = "tetrad-update.json.sig";
/// The base64 Ed25519 key that signs releases. The release workflow checks that its secret
/// key matches this value. A new key needs one install with `scripts/install.sh`.
pub const RELEASE_KEY: &str = "M4g350Lncizx/83Q9fJVpFZ9oiHvkr/1/2D5fiB8I2o=";
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
const MAX_BINARY_BYTES: u64 = 64 << 20;
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

/// What [`MANIFEST`] says about one file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: Version,
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
        let bad = TetradUpdateError::Signature;
        let decode = |text: &[u8]| {
            let text = std::str::from_utf8(text).map_err(|_| bad("not UTF-8"))?;
            BASE64.decode(text.trim()).map_err(|_| bad("not base64"))
        };
        let key: [u8; 32] = (decode(key.as_bytes())?.try_into()).map_err(|_| bad("bad key"))?;
        let key = VerifyingKey::from_bytes(&key).map_err(|_| bad("bad key"))?;
        let signature =
            Signature::from_slice(&decode(signature)?).map_err(|_| bad("not 64 bytes"))?;
        (key.verify_strict(json, &signature)).map_err(|_| bad("does not match the release key"))?;
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

/// The newest release in a GitHub release list that is newer than `current` and has
/// [`MANIFEST`]: its tag and version. A pre-release counts only when `current` is a
/// pre-release too, so a stable Tetrad stays on stable releases.
pub fn newest(
    json: &[u8],
    current: &Version,
) -> Result<Option<(String, Version)>, TetradUpdateError> {
    let releases: Vec<Release> =
        serde_json::from_slice(json).map_err(|e| TetradUpdateError::Releases(e.to_string()))?;
    Ok(releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter(|release| release.assets.iter().any(|asset| asset.name == MANIFEST))
        .filter_map(|release| {
            let version = release
                .tag_name
                .strip_prefix('v')?
                .parse::<Version>()
                .ok()?;
            let channel = version.pre.is_empty() || !current.pre.is_empty();
            (channel && &version > current).then_some((release.tag_name, version))
        })
        .max_by(|a, b| a.1.cmp(&b.1)))
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

/// The newest installable release that is newer than `current`, with its checked manifest.
pub fn check(
    http: &mut impl HttpClient,
    current: &Version,
) -> Result<Option<Offer>, TetradUpdateError> {
    let url =
        format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page={RELEASES_PER_PAGE}");
    let Some((tag, version)) = newest(&get(http, url, MAX_RELEASES_BYTES)?, current)? else {
        return Ok(None);
    };
    let json = get(http, asset_url(&tag, MANIFEST), MAX_MANIFEST_BYTES)?;
    let signature = get(http, asset_url(&tag, SIGNATURE), MAX_SIGNATURE_BYTES)?;
    let manifest = Manifest::verify(&json, &signature, RELEASE_KEY, Some(&version))?;
    Ok(Some(Offer {
        source: Source::Release(tag),
        manifest,
    }))
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
        for path in FILES {
            let _ = files.remove(&part(path));
        }
        return staged;
    }
    // The commit: from here the new files are in place. A failed arm leaves `select` with the
    // old md5, so the launcher starts stock CorOS until `tetrad-launcher.sh arm` runs. The
    // downloads are synced first, so a power cut never leaves a renamed file half written.
    files.sync()?;
    for path in FILES {
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
    let parts: Vec<String> = FILES
        .iter()
        .map(|path| part(path).display().to_string())
        .collect();
    run(
        runner,
        &Command::new(
            "chmod",
            ["755"].into_iter().chain(parts.iter().map(String::as_str)),
        ),
    )?;
    for (path, file) in FILES.iter().zip(&parts).skip(1) {
        if !runner
            .run(&Command::new("sh", ["-n", file.as_str()]))?
            .status
            .success()
        {
            return Err(TetradUpdateError::Script((*path).to_owned()));
        }
    }
    let check = Command::new(&parts[0], ["run", "--check"]).cwd(SCRIPT_DIR);
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

    /// The newest release with a manifest wins; older ones, releases without a manifest and
    /// drafts do not count. A stable Tetrad never takes a pre-release.
    #[test]
    fn newest_follows_semver_and_channel() {
        let mut draft = release("v0.3.0", true);
        draft["draft"] = true.into();
        let list = serde_json::json!([
            draft,
            release("v0.2.0", false),
            release("v0.1.0", true),
            release("v0.2.0-alpha.1", true),
            release("v0.1.0-alpha.3", true),
            release("nightly", true),
        ])
        .to_string();
        let pick = |current| newest(list.as_bytes(), &v(current)).unwrap().map(|r| r.0);
        assert_eq!(pick("0.1.0-alpha.3").as_deref(), Some("v0.2.0-alpha.1"));
        assert_eq!(pick("0.0.9").as_deref(), Some("v0.1.0"));
        assert_eq!(pick("0.2.0-alpha.1"), None);
        assert_eq!(pick("0.1.0"), None);
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

    /// The happy path: checked `.part` files move into place, then `arm` runs.
    #[test]
    fn install_moves_checked_files_then_arms() {
        let files = MemFiles::default();
        files.put("/media/p4/tetrad/bin/tetrad", "old");
        let mut http = served(&files, [b"new-elf", b"sh", b"sh", b"sh"]);
        let mut runner = ScriptedRunner::default();
        checks(&mut runner, 0);
        runner.expect_ok("sh /media/p4/tetrad/launcher/tetrad-launcher.sh arm");
        let offer = offer(b"new-elf", b"sh");
        install(&mut http, &mut files.clone(), &mut runner, &offer).unwrap();
        assert!(runner.is_done());
        assert_eq!(
            files.get("/media/p4/tetrad/bin/tetrad").as_deref(),
            Some(&b"new-elf"[..])
        );
        assert_eq!(files.get("/media/p4/tetrad/bin/tetrad.part"), None);
        assert_eq!(
            http.requests[0].url,
            "https://github.com/JudahZF/tetrad/releases/download/v0.2.0/tetrad"
        );
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
