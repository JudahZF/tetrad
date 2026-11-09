# Tetrad: rules for agents

Tetrad is a replacement touch UI for the Neural DSP Quad Cortex, written in Rust. It runs as
root on the unit's 32-bit ARM CPU, next to the stock kernel, drivers and DSP firmware. A bug can
stop the sound on stage, damage a user's library or stop the unit from booting.

Read these before you change code:

- [docs/architecture.md](docs/architecture.md): the crate map and the rules for the code.
- [docs/development.md](docs/development.md): build, test and release.
- [.cursor/BUGBOT.md](.cursor/BUGBOT.md): what a review looks for.
- [mobile/AGENTS.md](mobile/AGENTS.md): the phone app in `mobile/` (Expo).

## Checks

Run these before you say that a task is done. CI runs the same commands.

| Purpose | Command |
| --- | --- |
| Submodules (once) | `git submodule update --init --recursive` |
| Build | `cargo build --locked --workspace --all-targets` |
| Test | `cargo test --locked --workspace` |
| Clippy | `cargo clippy --locked --workspace --all-targets -- -D warnings` |
| Format | `cargo fmt --all -- --check` |
| Rustdoc | `RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps` |
| Dev feature | `cargo test --locked -p tetrad-app -p tetrad-ui --features dev` |
| Phone app | `cd mobile && pnpm install --frozen-lockfile && pnpm typecheck && pnpm lint && pnpm test` |

- Always use `--locked`.
- Some tests read the published extensions. Run `scripts/fetch-extensions.sh /tmp/extensions`
  and set `TETRAD_EXTENSIONS_DIR=/tmp/extensions` (an absolute path). Without it they skip.
- Tests marked `#[ignore]` need stock files from a unit (`QC_CORPUS_DIR`). Do not run them
  and do not remove the mark.
- `cargo build` makes a host binary. Only the device build
  (`nix develop path:./qc-dev -c ./qc-dev/build-arm.sh . dist`) runs on a unit.

## Rules

- **No stock bytes in the repository.** No ZenUI or SHARC binaries, decrypted blobs,
  databases, presets from a unit or device keys. Tests use synthetic data.
- **Lossless data.** Presets, settings and Cortex Control payloads keep unknown fields and the
  original bytes. A save without a change gives the same bytes.
- **Injected I/O.** Library crates take traits or closures for files, devices, sockets, clocks
  and commands. Only `tetrad-sys` has Linux backends. `unsafe` is forbidden outside `tetrad-sys`.
- **One owner of state.** The device state changes in the reducer. Events go in, effects come
  out. Do no blocking I/O, sleep or network call on the device loop; use the service workers.
- **32-bit target.** `usize` is 32 bits on the unit. Do not cast `u64` to `usize` without a
  bound. The unit has little RAM: put a limit on every buffer, queue and file read.
- **Boot scripts.** `scripts/launcher/*.sh` run on BusyBox ash 1.23: POSIX sh only, no
  bashisms, no GNU-only flags. A change that can stop the fall-back to stock CorOS is a blocker.
- **`ZenUI` name first.** `tetrad run`, `tetrad bringup` and `tetrad select` take the process
  name `ZenUI` before any slow work. The watchdog restarts the unit after about 3 s without it.
- **Signatures.** Check the signature and the SHA-256 of an update, a catalog or a package
  before you install, run or trust it.
- **Plugin licences.** The licence checks are deliberate. Never weaken, bypass or remove one.
- **Themes.** Screens take colours from `l.palette`, never from colour constants.
- **Extension API.** A new manifest field, capability or host call needs a new API version.
- **Comment marks.** **(C)** is confirmed stock behaviour, **(I)** is inferred and **(O)** is
  an owned choice. Mark new behaviour that copies or differs from stock.

## Style

- Rust edition 2024, Rust 1.95. Clippy runs with `-D warnings`.
- Keep it simple. Add a test only when it guards a contract that will matter for a long time.
- Comments say why, not what. Keep comments and docs true when you change code.
- Docs, the changelog and UI text use Simplified Technical English: short sentences, one term
  for one thing, plain words.
- A change that a user can see needs an entry under `## Unreleased` in `CHANGELOG.md` (Added,
  Changed or Fixed) and an update of its page in `docs/`.
- Commits use Conventional Commits with a scope and say what the user sees, for example
  `fix(tuner): stop the Tuner flashing when there is no input`.
