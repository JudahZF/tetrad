# Default extension repository

This folder is a ready-to-publish copy of the default Tetrad extension repository,
[`JudahZF/tetrad-extensions`](https://github.com/JudahZF/tetrad-extensions). Tetrad devices
know this repository and its key from the firmware (`DEFAULT_SOURCE` and `DEFAULT_KEY` in
`crates/tetrad-extensions/src/repository.rs`).

| File | Where it goes | What it is |
| --- | --- | --- |
| `tetrad-extensions.json` | default branch, repository root | The catalog: every package with its release tag, asset name, size and SHA-256. |
| `tetrad-extensions.sig` | default branch, repository root | Base64 Ed25519 signature over the exact catalog bytes. |
| `tetrad-extensions.pub` | default branch, repository root | Base64 public key. Devices show its fingerprint when a user adds a repository. |
| `packages/*.tetradext` | release assets of the tag in the catalog | The packages. Do not commit them to the branch. |

The device reads `https://raw.githubusercontent.com/<owner>/<repo>/HEAD/tetrad-extensions.{json,sig}`,
verifies the signature with its pinned key, and downloads each package from
`https://github.com/<owner>/<repo>/releases/download/<tag>/<asset>`. It refuses a package
whose size or SHA-256 differs from the signed catalog.

## Signing key

The maintainer keeps the private key of the default repository outside every repository
(mode 0600); the commands below call it `$KEY`. Never copy it into a repository.
Fingerprint: `c702 baa2 2830 5c13 b3e6 e9a1 bda9 277a`.

If the key is lost, devices cannot verify a new catalog until a firmware update changes
`DEFAULT_KEY`. A custom repository makes its own key with `tetrad-ext keygen --out DIR`.

## Publish a release

From the Tetrad workspace root, with `T=target/debug/tetrad-ext` (`cargo build -p tetrad-extension-cli`):

1. Build each changed extension (type-check, then bundle):

   ```sh
   (cd extensions/midi-mapper && npm install)
   $T build extensions/midi-mapper
   ```

2. Package it into `packages/`. Increase `version` in `tetrad-extension.json` first: a
   published `<id>-<version>` must never change.

   ```sh
   $T package extensions/midi-mapper --out extensions/repository/packages/tetrad.midi-mapper-0.2.0.tetradext
   ```

3. Write and sign the catalog with a new release tag. Packages already in the catalog with
   the same bytes keep their old tag; new packages get the new one.

   ```sh
   $T catalog --repo-dir extensions/repository \
     --key "$KEY" \
     --tag extensions-2026-10-20
   ```

4. In `JudahZF/tetrad-extensions`, create the release for the new tag and upload the new
   `.tetradext` files as its assets:

   ```sh
   gh release create extensions-2026-10-20 --repo JudahZF/tetrad-extensions \
     extensions/repository/packages/tetrad.midi-mapper-0.2.0.tetradext
   ```

5. Then commit `tetrad-extensions.json`, `tetrad-extensions.sig` and `tetrad-extensions.pub`
   to the default branch. Upload the assets first, so that no device sees a catalog entry
   whose asset does not exist yet.

Check a package at any time with `$T check FILE.tetradext`.

## Current contents

| Package | Tag |
| --- | --- |
| `tetrad.midi-mapper-0.2.0.tetradext` (trigger provider for Mappings) | `extensions-2026-10-20` |
| `tetrad.nam-0.1.0.tetradext`, `0.2.0`, `0.3.0` (Neural Amp Modeler blocks, API 2; 0.3.0 adds trainer WaveNets) | `extensions-2026-10-23` |
| `tetrad.nam-0.3.1.tetradext` (WaveNets past the network limits, such as kernels above 16, stay on the scalar graph) | `extensions-2026-10-23.2` |
| `tetrad.dsp-examples-0.1.0.tetradext`, `0.2.0` (qcdsp example blocks, API 2) | `extensions-2026-10-23` |
| `tetrad.dsp-examples-0.3.0.tetradext` (without the synth, which moved to Sweep Synth) | `extensions-2026-10-24` |
| `tetrad.sweep-synth-0.1.0.tetradext` (mono guitar and bass synth blocks, API 2) | `extensions-2026-10-24` |
| `tetrad.midi-mapper-0.3.0.tetradext` (note-offs release their note, API 3) | `extensions-2026-10-24.2` |
| `tetrad.custom-footswitch-0.1.0.tetradext` (a 4-page Custom footswitch mode, data only, API 4) | `extensions-2026-10-24.3` |
| `tetrad.nam-0.5.0.tetradext` (Neural Amp Modeler A1, with the NAM plugin's tone controls, API 5) | `extensions-2026-10-27` |
| `tetrad.nam-a2-0.2.0.tetradext` (Neural Amp Modeler A2, plays A2-Lite, API 5) | `extensions-2026-10-27` |

Devices before API 2 skip the API 2 packages, and so on for each API level (API 5 is the
leaky ReLU activation of Neural Amp Modeler A1 0.5.0 and A2 0.2.0):
`latest()` filters on the API version. The
example graphs come from `cargo qcdsp export --example <name> --output <file>` (the slapback is
`--example delay --delay-samples 3600`). Sweep Synth's graphs are `--example sweep-synth` and
`--example bass-sweep-synth`.
