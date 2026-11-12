# Releases and channels

Tetrad has two release channels. Both are GitHub releases of `JudahZF/tetrad`, built and signed
by `.github/workflows/release.yml` in the same way.

| Channel | Built from | GitHub release | Version | Who should use it |
| --- | --- | --- | --- | --- |
| **Stable** | A `v*` tag | The tag. `vX.Y.Z` is "latest". `vX.Y.Z-beta.N` and `vX.Y.Z-rc.N` are pre-releases | The tag without `v` | Everyone |
| **Nightly** | `main`, once a day | One rolling pre-release, tag `nightly`, replaced each night. Never "latest" | `X.Y.Z-nightly.DATE.TIME+COMMIT` | Testers who accept a broken build |

## Policy

- **Stable** is tested on a unit before the tag. Use it on stage.
- **Nightly** is the newest `main`, built with no test on a unit. It can stop the sound, fail to
  start or have bugs that a stable does not have. Do not use it on stage. Keep a backup of your
  library ([support.md](support.md)).
- The launcher protects both in the same way: a binary that does not start three times in a
  row is turned off, and the unit starts stock CorOS ([launcher.md](launcher.md)).
- The default channel is stable. A unit with no channel setting, or with a setting that it does
  not know, uses stable.

### Roll back from a nightly

1. Use the stable channel. When a stable is newer than your nightly, Tetrad Updates offers it.
2. A stable that is older than your nightly is not offered: an update never goes to an older
   version. Install it over USB (`tetrad-usb install`, any signed version installs, an older
   one too: [usb.md](usb.md#install-a-release-over-usb)) or over SSH with the release archive
   (`scripts/install.sh update`, [install.md](install.md)).
3. When the nightly does not start, the launcher starts stock CorOS. Install a stable over USB
   or SSH as in step 2.

Old nightlies are not kept. To test an older `main`, build it ([development.md](development.md)).

## Versions and tags

- **Stable tag:** `v` and the version in `Cargo.toml` (`[workspace.package] version`), for
  example `v0.1.0` or `v0.2.0-rc.1`. The workflow stops when they differ. A tag with a hyphen is
  published as a pre-release.
- **Nightly version:** `X.Y.Z-nightly.DATE.TIME+COMMIT`, for example
  `0.1.1-nightly.20261112.317+61697fa`. `DATE` is `YYYYMMDD` and `TIME` is `HHMM` in UTC with no
  leading zero (03:17 is `317`). `COMMIT` is the short commit of `main`.
  - When `Cargo.toml` has a stable version (`0.1.0`), `X.Y.Z` is the next patch (`0.1.1`).
  - When it has a pre-release (`0.2.0-alpha.1`), `X.Y.Z` is its core (`0.2.0`).
  - So by semver precedence a nightly is newer than the version in `Cargo.toml` and older than
    the next stable. A later nightly is newer than an earlier one, also on the same day. The
    build metadata (`+COMMIT`) does not count in a comparison.
  - Limit: `nightly` sorts before `rc`, so a nightly of `0.2.0` is older than `0.2.0-rc.1`.
    Only a unit that changes channel sees this.
  - The build writes this version into `Cargo.toml` and `Cargo.lock`, so the nightly binary
    reports it (`CARGO_PKG_VERSION`), as a stable binary reports its tag.
- Never give a nightly a `v` tag. Units up to 0.1.0-alpha.10 would offer it as an update
  (see below).

`scripts/release.sh` has the version and manifest steps. `cargo test --locked -p tetrad-services
--test release_script` checks them against this page.

## The update contract

This is what a unit reads. Change it only with a plan for the units in the field.

### Assets

Each release has the same assets: `tetrad`, `tetrad.sha256`, `tetrad.md5`,
`tetrad-<tag>-armv7.tar.gz` and its `.sha256`, `tetrad-launch.sh`, `tetrad-launcher.sh`,
`tetrad-update.sh`, `tetrad-update.json` and `tetrad-update.json.sig`. A unit downloads them from
`https://github.com/JudahZF/tetrad/releases/download/<tag>/<name>`.

### The manifest (`tetrad-update.json`)

```json
{
  "version": "0.1.1-nightly.20261112.317+61697fa",
  "channel": "nightly",
  "files": {
    "bin/tetrad": { "size": 18080000, "sha256": "<64 lowercase hex digits>" },
    "launcher/tetrad-launch.sh": { "size": 10680, "sha256": "…" },
    "launcher/tetrad-launcher.sh": { "size": 6749, "sha256": "…" },
    "launcher/tetrad-update.sh": { "size": 8186, "sha256": "…" }
  }
}
```

| Key | Stable | Nightly | Meaning |
| --- | --- | --- | --- |
| `version` | yes | yes | The semver version that the binary reports. It must be the version of the tag (stable) |
| `channel` | **no** | `"nightly"` | The channel. A manifest with no `channel` is stable |
| `files` | yes | yes | Exactly the four paths above, each with its size (1 byte to 64 MiB for the binary, 256 KiB for a script) and SHA-256 |

- A stable manifest has no `channel` key. Units up to 0.1.0-alpha.10 parse the manifest with
  `deny_unknown_fields`: a key they do not know makes the update check fail. Add `channel` to
  stable manifests only after those units are gone.
- The same `channel` key keeps those units from installing a nightly over USB.
- `tetrad-update.json.sig` is the base64 Ed25519 signature of the exact bytes of the manifest,
  by the key that `RELEASE_KEY` in `crates/tetrad-services/src/updater/tetrad.rs` pins. Both
  channels use the same key (the `TETRAD_RELEASE_KEY` secret). A unit checks the signature
  before it reads the manifest, then the size and SHA-256 of each file, then that the scripts
  parse and that the new binary supports the running CorOS (`tetrad run --check`).
- The list of files does not change: units up to 0.1.0-alpha.10 refuse another list.

### How a unit finds an update

All units read `https://api.github.com/repos/JudahZF/tetrad/releases?per_page=30`: the newest 30
releases. They skip drafts and releases without `tetrad-update.json`.

**Stable channel.** Units up to 0.1.0-alpha.10 have only this channel, and it is the default of
later units.

1. Take each release whose tag is `v` and a semver version. Other tags (`nightly`) do not count.
2. A pre-release version counts only when the running version is a pre-release too. The unit
   reads this from the version, not from the GitHub pre-release flag.
3. Take the newest version that is newer than the running version.
4. Download the manifest and its signature. The manifest `version` must equal the tag version.

**Nightly channel.**

1. Take each release whose tag is `nightly` or starts with `nightly-`. Today there is only
   `nightly`.
2. Take the newest by `published_at`.
3. Download the manifest and its signature. The manifest must have `"channel": "nightly"`, and
   its first pre-release identifier is `nightly`.
4. Offer it when its version is newer than the running version by semver precedence.

### Rules that keep units in the field working

- Only stable releases have `v` tags. A `v` nightly is offered to every unit on a pre-release.
- Keep the stable manifest to `version` and `files`.
- Keep at most a few releases that are not `v` tags. Units read only the newest 30 releases, so
  many nightlies would hide the stable releases. This is why the nightly is one rolling
  release and not one release per day.
- Keep the asset names, the four files and the release key.

## The nightly run

The workflow runs at 03:17 UTC each day on `main`. It does nothing when the `nightly` tag is
already the newest commit of `main` and its release has a signature. Else it:

1. makes the nightly version and stamps it into `Cargo.toml` and `Cargo.lock` (only the
   versions of the workspace crates change),
2. builds and checks the binary as for a stable,
3. writes and signs the manifest with `"channel": "nightly"`,
4. deletes the old `nightly` release and tag, and publishes the new release at the commit as a
   pre-release that is not "latest". `gh release create` publishes only after all assets are
   uploaded. Between the delete and the new release, there is no nightly.

A manual run (Actions → release → Run workflow) is a dry run: it builds a nightly and uploads a
workflow artifact, with no signature and no release. With `publish` on `main`, it publishes the
nightly at once, also when nothing changed. A dry run on another branch never signs: a unit
installs any signed version over USB.

## Cut a stable release

1. On a branch, set `version` in `[workspace.package]` of `Cargo.toml`, for example `0.1.0`,
   and run `cargo update --workspace` to update `Cargo.lock`.
2. In `CHANGELOG.md`, rename `## Unreleased` to `## [0.1.0](https://github.com/JudahZF/tetrad/releases/tag/v0.1.0) (YYYY-MM-DD)`
   and add a new empty `## Unreleased` above it.
3. Merge the pull request. Check the build on a unit (see Policy).
4. Tag the merge commit and push the tag:

   ```sh
   git tag -a v0.1.0 -m "Tetrad v0.1.0" <commit>
   git push origin v0.1.0
   ```

5. The workflow builds, signs and publishes the release. Check that `tetrad-update.json` has no
   `channel` key and that the release is "latest" (a plain `vX.Y.Z`) or a pre-release (a tag
   with a hyphen).

From the next night, the nightly version is `0.1.1-nightly.…`. For a beta or a release
candidate, use the same steps with `0.2.0-beta.1` or `0.2.0-rc.1`.
