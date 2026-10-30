# Bugbot rules for Tetrad

Tetrad is a replacement touch UI for the Neural DSP Quad Cortex, in Rust. It runs as root on the
unit's 32-bit ARM CPU (armv7), next to the stock kernel, drivers and DSP firmware. A bug can stop
the sound on stage, damage a user's library, or stop the unit from booting. Read
[docs/architecture.md](../docs/architecture.md) for the crate map and the rules for the code.

CI already runs build, tests, Clippy with `-D warnings`, rustfmt and rustdoc. Do not report
style, formatting or lint problems that these catch. Report real defects.

## High severity

- **Boot and recovery.** `scripts/launcher/*.sh` decide at each boot between Tetrad and stock
  CorOS, and fall back to CorOS after failed starts. A change that can stop the fall-back, or
  leave the unit with no working UI, is a blocker. These scripts run on BusyBox ash 1.23: flag
  bashisms (`[[ ]]`, arrays, `local -n`, `$'…'`, `<<<`, `pipefail`) and GNU-only flags.
  `scripts/install.sh` runs on the user's computer in POSIX sh (Linux and macOS).
- **ZenWatchdog.** `tetrad run`, `tetrad bringup` and `tetrad select` must take the process name
  `ZenUI` first. The watchdog restarts the unit after about 3 s without it. Flag new work that
  runs before the rename.
- **Lossless data.** Presets, settings and Cortex Control payloads keep unknown fields and the
  original bytes, and a no-op save gives the same bytes. Flag any read-modify-write path that
  rebuilds a message from known fields only, drops hidden or unknown parameters, reorders
  fields, or changes float encodings. A past bug dropped hidden parameters in this way.
- **Stock files stay on the unit.** The repository holds no stock bytes: no ZenUI or SHARC
  binaries, no decrypted blobs, no `zenui.db`, `factory.db` or presets from a unit, no device
  keys. Tests use synthetic data, and tests that need stock files are `#[ignore]` and read
  `QC_CORPUS_DIR`. Flag any new fixture that looks like stock data.
- **Signatures.** Release updates, extension catalogs and packages are signed. Flag any path that
  installs, runs or trusts one of them before its signature and SHA-256 are checked, or that
  accepts a key other than the configured one. The Remote update of a dev build (`--features
  dev`) is unsigned by design ([docs/remote.md](../docs/remote.md)): do not report that.
- **Remote (LAN HTTP server).** Every call that reads device data or does an action needs the
  PIN. Only the page itself and, in dev builds, the key handshake (`/api/auth/nonce`,
  `/api/auth/pin`) work without it. Flag a missing check, path traversal in file names, folder
  names and archive entries, uploads without a size limit, and responses that leak the serial,
  tokens, passwords or account names (support file and notices included).

## Device constraints

- `usize` is 32 bits on the unit. Flag `u64` to `usize` casts, offsets and sizes that can
  overflow on armv7 but not on the 64-bit host where the tests run.
- The device loop draws the UI and handles the footswitches. Flag blocking I/O, sleeps, network
  calls or heavy work on that loop; services run on worker threads.
- The unit has little RAM. Flag unbounded buffers, queues and caches, and files read whole into
  memory without a limit.

## Architecture rules

- **Injected I/O.** Crates take traits or closures for files, devices, sockets, clocks and
  commands. Only `tetrad-sys` holds Linux backends. Flag direct `std::fs`, `std::net`,
  `std::process` or clock reads in other library crates when the crate already injects that
  kind of I/O.
- **One owner of state.** Device state changes in the reducer. Events go in, effects come out.
  Flag state changes from outside the reducer.
- **`unsafe`** is forbidden in every crate except `tetrad-sys` (workspace lint). Flag a crate
  that opts out of the workspace lints.
- **Themes.** Screens take colours from `l.palette`, never from colour constants. A stock shade
  that is not a palette role is drawn as `p.tone(CONST)`. Block colours, scene swatches and the
  tuner green are data and stay fixed.
- **Extension API.** A new manifest field, capability or host call needs a new API version, and
  a new package layout needs a new package format. Older Tetrad builds must refuse packages that
  they cannot run correctly. Flag a feature that ships without the bump, or a bump that breaks
  packages that already work.
- **Comment marks.** **(C)** is confirmed stock behaviour, **(I)** is inferred and **(O)** is an
  owned choice. Flag new behaviour that copies stock without a mark, and addresses such as
  `0x5239d0` that are used as code instead of as a reference.

## Docs and changelog

- A user-visible change needs an entry under `## Unreleased` in `CHANGELOG.md` and an update to
  the page in `docs/` that describes it. Flag docs that no longer match the code.
- Docs use Simplified Technical English: short sentences, one term for one thing, plain words.
