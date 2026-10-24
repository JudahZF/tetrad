# Extensions

Extensions add small programs and DSP blocks to Tetrad. You install them on the unit from a
repository on GitHub. An extension is written in TypeScript and runs in its own process, so a
slow or broken extension cannot stop your sound or the touch UI. A DSP block is a graph that
Tetrad itself compiles for the DSP: a package never brings DSP machine code.

- **MIDI Mapper** reserves a second MIDI channel and offers it as a trigger on the
  [Mappings](mappings.md) page.
- **Neural Amp Modeler A1** plays Neural Amp Modeler (NAM) LSTM and WaveNet models, including
  the NAM trainer's nano, feather and lite sizes, as amp blocks.
- **Neural Amp Modeler A2** plays NAM A2 models (A2-Lite) as amp blocks.

- [Use extensions](#use-extensions)
- [DSP blocks](#dsp-blocks)
- [Write an extension](#write-an-extension)
- [Write a DSP block](#write-a-dsp-block)
- [Publish a repository](#publish-a-repository)

## Use extensions

Open **Settings → System → Extensions** (the last row).

### Install

1. Tap **REFRESH**. Tetrad reads the catalog of every trusted repository.
2. Under **Available**, tap an extension, then **INSTALL**.

Keep each extension on from one repository only. Two installations that are on add the same
blocks and themes twice, and both run their code. Tetrad warns: an Available row says **Also
installed from** the other repository, and while both are on, each Installed row says **Also on
from** the other one. Turn one off, or remove it.

Installed extensions start at every boot. They do not need the network after they are
installed.

### Repositories

The default repository is [JudahZF/tetrad-extensions](https://github.com/JudahZF/tetrad-extensions).
Its signing key is built into Tetrad (fingerprint `c702 baa2 2830 5c13 b3e6 e9a1 bda9 277a`).

To add another repository, tap **REPOSITORIES → ADD REPOSITORY** and type its `owner/repo`.
Tetrad shows the fingerprint of the repository's signing key and asks you to trust it. Trust
only repositories that you know: an extension runs code on your unit. After you trust a key,
Tetrad installs only packages that this key signed, and it refuses a catalog if the key
changes.

### Settings, commands and faults

Tap an installed extension to see its page:

- **Enabled** starts or stops it. A stopped extension frees its MIDI channel.
- **Settings** come from the extension (for example MIDI Mapper's channel). A change restarts
  the extension.
- **Commands** are buttons that the extension adds.
- **Status** and **Log** show what the extension reports.
- **Faulted** means that Tetrad stopped the extension. The page shows the reason; tap **RETRY**
  to start it again. Common reasons: the extension used too much memory or CPU time, or it
  asked for a MIDI channel that the QC itself uses.
- **REMOVE** deletes the extension. Its settings and stored data stay, so a new install keeps them.

### MIDI Mapper

MIDI Mapper takes one MIDI channel (default channel 2). Messages on that channel go only to
MIDI Mapper, not to the QC's own MIDI map. It sends each CC, note and program change to
Mappings as a trigger, for example "MIDI Mapper · Ch 3 / CC 21". Choose what each one does on
the Mappings page.

Choose a channel that is not the QC's own MIDI channel (Settings → Device → MIDI). If they are
the same, the extension faults with that reason.

### Limits

Each extension runs in a separate low-priority process with a 128 MiB address-space limit, a
32 MiB JavaScript heap, 50 ms of CPU time for each piece of work, 1 s for an event handler and
3 s to start. When it builds a DSP block from a file, the limits are 2 s of CPU time and 5 s.
Tetrad stops an extension that goes past a limit.

## DSP blocks

> [!WARNING]
> DSP blocks are experimental. On the test unit, the output of the example and NAM blocks is
> within 2e-6 of their host references, but each new block is new code on the DSP. Try it at a
> low volume.

A package can bring DSP blocks. When the extension is installed and **Enabled**, Tetrad
compiles its blocks for the DSP, and each block shows in the block list of its category, like a
stock model. You edit it with the knobs and the screen, and you can use it in scenes, stomps
and [Mappings](mappings.md).

- The extension's page lists its **DSP blocks**, with the CPU that each one uses on a DSP core,
  or the reason that a block is not ready.
- **Presets.** When Tetrad saves a preset, a DSP block becomes a stock block in its slot (the
  package chooses which; the default is the stock Gain block at 0 dB). Tetrad also writes a
  record of the DSP block into the preset. Stock CorOS loads the preset and plays the stock
  block. Tetrad loads the preset and plays the DSP block again.
- **Missing blocks.** If a preset uses a DSP block that is not installed (or its extension is
  off), the stock block plays. Install or enable the extension: the DSP block comes back at
  once. If you replace or remove the stock block in that slot first, the record goes at the
  next save.
- **CPU.** A DSP block uses DSP CPU like any other block. If a preset does not fit, Tetrad tells
  you, as for stock blocks. The CPU figures are estimates until they are measured on a unit.
- Cortex Control gets the stock block when it reads the whole preset. A block edit on the
  unit while Cortex Control is connected can show there as an unknown block.

### Neural Amp Modeler

Two extensions play [NAM](https://www.neuralampmodeler.com/) models. Both are `.nam` files, so
look at the model's page (for example on TONE3000) to see which one you have:

| Extension | Plays | Asset folder | Blocks |
| --- | --- | --- | --- |
| **Neural Amp Modeler A1** | LSTM and WaveNet models, up to the NAM trainer's **lite** size | `/media/p4/tetrad/assets/tetrad.nam/` | **NAM A1 …** |
| **Neural Amp Modeler A2** | A2 models (NAM's default since June 2026) | `/media/p4/tetrad/assets/tetrad.nam-a2/` | **NAM A2 …** |

An A2 file holds two sizes of the same model: A2-Full and A2-Lite. Neural Amp Modeler A2 plays
A2-Lite. A2-Full needs more DSP memory and CPU than one core has. A file in the wrong folder
does not load, and the extension's page says which extension plays it.

1. Install the extension from the default repository: on a Tetrad build with DSP blocks, open
   Settings → System → Extensions, push **REFRESH** and install it.
2. Copy your `.nam` files to the extension's asset folder on the unit, for example with
   `scp "Plexi.nam" qc:/media/p4/tetrad/assets/tetrad.nam-a2/`. Tetrad makes the folder at
   the first start. A file name can have letters, digits, spaces, `.`, `_` and `-`.
3. Turn the extension off and on (or restart Tetrad). Each file becomes a block, such as
   **NAM A2 Plexi**, in the **Guitar Amplifier** list.

Each block has an **Input** and an **Output** level (0 to 4, 1 is unchanged). The model runs in
mono: both channels get its output. The page of the extension says why a file did not load.
Models must be for 48 kHz. Each block runs on one DSP core, so a model must fit in one core's
CPU and memory. On the test unit:

| Model | CPU of one core | DSP memory |
| --- | --- | --- |
| LSTM, 2 layers of 6 | about 60% | small |
| WaveNet nano | below 25% | 57 KB |
| WaveNet feather | about 48% | 115 KB |
| WaveNet lite | about 75% | 129 KB |
| A2-Lite | about 29% (estimate) | 112 KB |

Lite needs most of a core: put it in a row whose core is otherwise light, or the preset does not
fit (as with a heavy stock amp). The trainer's **standard** size needs more DSP memory than one
core has, so Tetrad refuses it; its page says so. Gated WaveNets (not made by the trainer) play
only when they are small.

## Write an extension

Requirements: Node.js with npm (not for a package of DSP blocks only), and the `tetrad-ext` command
(`cargo install --path crates/tetrad-extension-cli`, or `cargo run -p tetrad-extension-cli
--bin tetrad-ext -- …`).

```sh
tetrad-ext init --name "My Extension" --id my.extension my-extension   # strict TypeScript + SDK types
cd my-extension && npm install
tetrad-ext build .               # tsc type-check, then esbuild to dist/index.mjs
tetrad-ext package --out ../packages .
```

### The manifest

`tetrad-extension.json` names the extension and declares what it may do:

```json
{
  "id": "my.extension",
  "name": "My Extension",
  "version": "0.1.0",
  "description": "What it does, in one sentence.",
  "api": 2,
  "capabilities": ["midiReceive", "triggers"],
  "settings": [
    { "key": "channel", "label": "MIDI channel", "kind": "choice", "default": 1,
      "options": [{ "value": 1, "label": "Channel 1" }, { "value": 2, "label": "Channel 2" }] }
  ],
  "commands": [{ "id": "reset", "label": "Reset" }]
}
```

Capabilities: `midiReceive`, `midiSend`, `controlsRead`, `controlsEdit`, `storage`,
`triggers`, `usb` and `blocks`. The host refuses every call that needs a capability the manifest
does not list. API 2 adds `blocks` ([Write a DSP block](#write-a-dsp-block)); an API 1 manifest
still works.
Setting kinds: `choice` (a dropdown; `default` is an option index), `toggle` and `number`
(`min`, `max`, `default`).

### The module

`src/index.ts` exports an `ExtensionDefinition`. `activate` may only read and register; work
that changes the device runs in handlers, which get a `scope`:

```ts
import type { ExtensionDefinition } from "@tetrad/extensions";

type Config = { channel: number };

const extension: ExtensionDefinition<Config> = {
  activate(ctx) {
    const { channel } = ctx.config;
    ctx.midi.reserveChannel(channel);
    ctx.triggers.declare({ id: "midi", label: `Ch ${channel}` });
    ctx.midi.onMessage(async (message, scope) => {
      if (message.kind !== "cc") return;
      await scope.triggers.fire({
        trigger: "midi",
        source: `cc:${message.controller}`,
        label: `CC ${message.controller}`,
        value: message.value / 127,
        continuous: true,
      });
    });
  },
};

export default extension;
```

The full API, with comments, is in `tetrad.d.ts` (`tetrad-ext sdk` writes it):

| Context (`activate`) | Scope (handlers) |
| --- | --- |
| `ctx.midi.reserveChannel`, `ctx.midi.onMessage` | `scope.midi.send` (CC and PC) |
| `ctx.commands.register` | `scope.controls.list`, `set`, `setBypass`, `toggleBypass` |
| `ctx.controls.list`, `ctx.controls.onChange` | `scope.storage.get`, `set`, `delete` |
| `ctx.lifecycle.onRunning`, `onPresetLoaded` | `scope.ui.setStatus` |
| `ctx.storage.get`, `ctx.triggers.declare`, `ctx.log/warn/error` | `scope.triggers.fire` |
| `ctx.usb.onMessage` (its return value is the reply; see [usb.md](usb.md)) | |

`ctx.controls.list` and `ctx.storage.get` work only during `activate`; handlers use their
scope. A scope closes when its handler settles. After a preset change, calls with an older scope fail
with `staleContext`. There is no module loader: bundle everything into one file (`tetrad-ext
build` does this).

### Ship themes

An extension can add up to 8 [themes](themes.md) to Settings → Appearance. List them in
`themes` in the manifest, in the same format as a theme file but without `version`:

```json
{
  "id": "my.theme-pack",
  "name": "My Themes",
  "version": "0.1.0",
  "api": 1,
  "themes": [
    { "id": "dusk", "name": "Dusk", "base": "slate",
      "colors": { "background": "#15111c", "accent": "#ff8fab" } }
  ]
}
```

A manifest with themes and no capabilities, settings or commands is **theme-only**:

- It needs no `src/` and no build. `tetrad-ext package` packs it without a module.
- Tetrad runs no code for it.
- It asks for no access.

An extension with code can add themes too.

`tetrad-ext package` checks the themes in the same way as the unit does. A theme that fails
(for example, a light background or text that is too hard to read) stops the package, and the
unit refuses it at install.

Themes show in Appearance while the extension is installed and enabled. The extension's page
lists them. See `extensions/fixtures/theme-pack` for a complete example.

Tetrad builds before 0.1.0-alpha.5 do not know the `themes` field. They refuse the package at
install and change nothing.

## Write a DSP block

A DSP block is a [qcdsp](../qcdsp/README.md) graph: an expression of the input sample,
parameters, constants, states and delay lines that runs once per sample. Operations: add,
subtract, multiply, divide, minimum, maximum, negate, absolute value, reciprocal, `tanh` and
sigmoid. The graph has no loops and no branches, so Tetrad knows its CPU and memory before it
loads it, and the block can only change the audio in its own slot. Tetrad compiles it on the
unit for the stock block framework of your CorOS build.

Declare each block in the manifest:

```json
"blocks": [
  { "id": "drive", "name": "Drive", "category": "guitar_overdrive", "placeholder": 3 }
]
```

`category` is one of `guitar_overdrive`, `guitar_amplifier`, `bass_amplifier`,
`bass_overdrive`, `equalizer`, `compressor`, `delay`, `modulation`, `reverb`, `wah`,
`utility`, `pitch`, `filter` and `synth`. `placeholder` is the stock model ID that presets
store for stock CorOS (default: the stock Gain block, 16005). A package has at most 16 blocks.

**A static block** has its graph in `blocks/<id>.qcdsp.json`. Make it with qcdsp's
`BlockBuilder` in Rust (`block.to_json()`), or start from the example that
`tetrad-ext init --blocks` writes. A package of static blocks only needs no TypeScript.

**A generated block** (`"generated": true`, capability `blocks`) is built by the extension from
a file. Add `"asset": { "label": "NAM model", "extensions": [".nam"] }`: Tetrad reads each such
file from `/media/p4/tetrad/assets/<extension id>/` and asks the extension for its graph. Each
file becomes a block named after the block and the file. A file is UTF-8 text of 3 MiB at most,
and at most 4 MiB less 16 KiB as a JSON string (a control character takes 6 bytes there). A file
whose name matches the extensions of more than one block builds each of these blocks.

```ts
ctx.blocks.provide("drive", (asset) => {
  const dsp = ctx.blocks.builder();
  const gain = dsp.parameter("Gain", 1, 40, 8);
  const out = dsp.tanh(dsp.mul(dsp.input(), gain));
  return dsp.finish(out);
});
```

The builder has the operations above, plus `state(initial)`, `delay(samples)`,
`updateState`, `updateDelay` and `mono()` (compute one channel and copy it: half the CPU).
Tetrad validates every graph and replaces its name and category from the manifest.

For convolutional models, `dsp.network()` starts a **network** (qcdsp IR 3): vector operations
on channel buffers that run before the graph, over a whole 32-sample call on the DSP. `input()`
is the block input; `buffer(channels)`, `gain(buffer, parameter)`, `conv(input, output, {kernel,
dilation, weights, bias, accumulate})` (a causal dilated convolution; kernel 1 is a dense layer),
`activation(buffer, "tanh" | "hardtanh" | "relu" | { leakyRelu: slope })` (a leaky ReLU,
`max(x, slope · x)`, slope 0 to 1), `add` and `copy` build it, and
`output(buffer)` returns its output as a signal for the graph. A block with a network is mono.
The DSP code computes two output channels per instruction (SIMD); Neural Amp Modeler's WaveNets
use it (`extensions/nam/src/wavenet.ts`, which Neural Amp Modeler A2 shares).

Limits of one block: 65 536 nodes (24 576 after `tanh` and the other approximations expand),
256 parameters, and 4096 states and delay lines. Up to 32 KB of state and delay memory lives in
the block itself; a block that needs more gets it from a DSP core heap when it starts: the fast
DM heap (about 150 KB per core, shared with the other blocks on that core) when the block fits
it, else the SD heap (16 MiB of DDR per core). Each load or store in the SD heap costs about 40
cycles, and the CPU estimate includes it, so the SD heap suits long delay lines with few taps.
The preset fit refuses a layout whose heaps or CPU do not fit, as for stock blocks; that check is
the only guard, because the DSP kernel does not refuse a request larger than its heap.
`tetrad-ext check <package>` shows each block's state size and CPU estimate. `tetrad-ext preview
--package <file> --block <id> --input in.wav --output out.wav --set Gain=12` runs a static
block on your computer.

### Try it on a unit

`tetrad-ext stage --repo-dir <repository> --source <owner/repo> --store <dir>/extensions`
builds an on-device store through the same verified install path the unit uses. Copy
`<dir>/extensions` to `/media/p4/tetrad/extensions` on the unit and restart Tetrad.

## Publish a repository

A repository is a GitHub repository with three files at the root of the default branch and the
packages as release assets:

| File | What it is |
| --- | --- |
| `tetrad-extensions.json` | The catalog: each package with its release tag, asset name, size and SHA-256. |
| `tetrad-extensions.sig` | The Ed25519 signature of the exact catalog bytes (base64). |
| `tetrad-extensions.pub` | The public key (base64). Units show its fingerprint when a user adds the repository. |
| Release `<tag>` assets | The `.tetradext` packages. |

```sh
tetrad-ext keygen --out keys     # once; keep keys/signing.key private and out of every repository
tetrad-ext package --out repo/packages my-extension
tetrad-ext catalog --repo-dir repo --key keys/signing.key --tag v1
```

Commit the three files, create the release `v1`, and attach the new packages. A unit
downloads `https://github.com/<owner>/<repo>/releases/download/<tag>/<asset>` and refuses a
package whose size or SHA-256 differs from the signed catalog. Never change a published
package: give each change a new version.

If you lose the signing key, units that trust your repository cannot verify a new catalog.
Users must remove the repository and trust a new key.
