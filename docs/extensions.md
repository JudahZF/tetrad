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
- **Sweep Synth** adds two synth blocks, one for guitar and one for bass, with a filter that
  sweeps on each note.
- **Custom Footswitch Mode** adds a fourth footswitch mode: four pages of ten switches, each
  with an action that you choose. See [footswitch-modes.md](footswitch-modes.md).
- **DOOM** plays the shareware episode of DOOM on the screen. The footswitches are the
  controls. See [DOOM](#doom).

- [Use extensions](#use-extensions)
- [DSP blocks](#dsp-blocks)
- [Write an extension](#write-an-extension)
- [Write a DSP block](#write-a-dsp-block)
- [Publish a repository](#publish-a-repository)

## Use extensions

Open **Settings → Tetrad → Extensions**.

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
  the extension. While the extension is on, its settings are also in the Settings menu: open
  the **Tetrad** group and tap the row with the extension's name.
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
the Mappings page. From version 0.3.0 a note-off releases its note, so a Momentary mapping on a
note holds its value while the key is down.

Choose a channel that is not the QC's own MIDI channel (Settings → Device → MIDI). If they are
the same, the extension faults with that reason.

### Sweep Synth

Sweep Synth and Bass Sweep Synth are in the Synth list. Each makes four voices from the note
you play: SUB OCTAVE (one octave down), GUITAR or BASS (the dry signal), OCTAVE (one octave up)
and SQUARE WAVE. Each note louder than TRIGGER restarts the filter: it moves from START FREQ to
STOP FREQ, as fast as RATE sets. RESONANCE gives the filter a peak, and ATTACK DELAY fades each
note in. The blocks play one note at a time, so play single notes and put them early in the
chain, before drive and modulation. They are mono: a stereo signal into them comes out with
its left channel on both sides.

DSP Examples 0.2.0 had a stereo copy of the guitar block, "qcdsp Micro Synth". DSP Examples
0.3.0 does not have it. After you update, a preset that uses it plays the stock Gain block
(0 dB) in that slot. Put Sweep Synth in the slot, set its controls again and save the preset.

### DOOM

DOOM is a [program](#ship-a-program): the original qcDOOM binary by Thomas Van Iseghem, with the
shareware WAD. Its Available row says **Runs a program with full access to the unit**. The
program is not confined as the TypeScript extensions are, so install it only from a repository
that you trust.

1. Install **DOOM** from the default repository (it needs a Tetrad build with extension API 7,
   after 0.1.0-alpha.9) and open its page.
2. Tap **Play DOOM**. DOOM takes the screen and the footswitches. Tetrad keeps the sound, MIDI,
   the volume knob and the expression pedals.
3. To quit, hold the screen for 2 seconds (or push the power button). Tetrad comes back.

| Switch | Action | Switch | Action |
| --- | --- | --- | --- |
| A | Turn left | E | Strafe left |
| B | Forward | F | Back |
| C | Turn right | G | Strafe right |
| D | Fire | H | Use |
| TEMPO | Menu | DOWN | Enter |

In the menu, A, B, C and F move and DOWN chooses. Saved games are in
`/media/p4/tetrad/assets/tetrad.doom/`.

### Limits

Each extension runs in a separate low-priority process with a 128 MiB address-space limit, a
32 MiB JavaScript heap, 50 ms of CPU time for each piece of work, 1 s for an event handler and
3 s to start. When it builds a DSP block from a file, the limits are 2 s of CPU time and 5 s.
Tetrad stops an extension that goes past a limit. A [program](#ship-a-program) has none of
these limits; it runs at nice 10.

## DSP blocks

> [!WARNING]
> DSP blocks are experimental. On the test unit, the output of the example and NAM blocks is
> within 2e-6 of their host references, but each new block is new code on the DSP. Try it at a
> low volume.

A package can bring DSP blocks. When the extension is installed and **Enabled**, Tetrad
compiles its blocks for the DSP, and each block shows in the block list of its category, like a
stock model. The **Extensions** row at the end of the block list also shows all blocks from
extensions, with the category of each. You edit a block with the knobs and the screen, and you
can use it in scenes, stomps and [Mappings](mappings.md).

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
   Settings → Tetrad → Extensions, push **REFRESH** and install it.
2. Copy your `.nam` files to the extension's asset folder on the unit. The easy way is the
   [Remote page](remote.md#extension-files): drop the files on it. Or copy them yourself, for example with
   `scp "Plexi.nam" qc:/media/p4/tetrad/assets/tetrad.nam-a2/`. Tetrad makes the folder at
   the first start. A file name can have letters, digits, spaces, `.`, `_` and `-`.
3. Turn the extension off and on (or restart Tetrad). Each file becomes a block, such as
   **NAM A2 Plexi**, in the **Guitar Amplifier** list.

Each block has the controls of the [Neural Amp Modeler plugin](https://github.com/sdatkinson/NeuralAmpModelerPlugin):

| Control | What it does |
| --- | --- |
| **Input** | A gain from 0 to 10 (1 is unchanged; the plugin's -20 to +20 dB is 0.1 to 10). |
| **Bass**, **Mid**, **Treble** | The plugin's tone stack, 0 to 10 with 5 flat: a low shelf at 150 Hz (4 dB per step), a peak at 425 Hz (3 dB per step; a cut is wider) and a high shelf at 1800 Hz (2 dB per step). |
| **Output** | -40 to +40 dB. |

The block plays the smallest size of an A2 file, A2-Lite: A2-Full needs more DSP memory and CPU
than one core has. The plugin's noise gate, loudness normalisation, calibration modes and EQ
switch, and its cab IR, are not in the block. The A1 extension has the same knobs.
NAM A1 0.5.0 and NAM A2 0.2.0 changed the knobs: in a preset saved with an older version, set
the knobs of each NAM block again.

The model runs in mono: both channels get its output. The page of the extension says why a file did not load.
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
`triggers`, `usb`, `remote` and `blocks`. The host refuses every call that needs a capability the
manifest does not list. API 2 adds `blocks` ([Write a DSP block](#write-a-dsp-block)); API 3 adds
`pressed` on trigger firings (see below); API 4 adds `footswitchMode`
([a footswitch mode](footswitch-modes.md#add-a-footswitch-mode-to-your-extension)); API 5 adds
the `leakyRelu` network activation ([Write a DSP block](#write-a-dsp-block)); API 6 adds `remote`
([a tab on the Remote page](#add-a-tab-to-the-remote-page)); API 7 adds `app`
([a program](#ship-a-program)). A manifest of an
older API still works. Use API 3 only to send releases: Tetrad before API 3 does not install
API 3 packages, because it would take a release for a press. Use API 5 for a block that uses
`leakyRelu`: Tetrad before API 5 refuses it. A package with a Remote page is format 3, which older
hosts do not read.
Setting kinds: `choice` (a dropdown; `default` is an option index), `toggle` and `number`
(`min`, `max`, `default`). The settings show on the extension's page and, while the extension
is on, in Settings → Tetrad under the extension's `name`.

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

A firing with `continuous: false` is a press. To release it, fire the same `source` again with
`pressed: false` (MIDI Mapper does this on a note-off). A mapping in Momentary mode holds its
value until the release; a second press does nothing. If an extension never sends a release, the
value stays held.

`ctx.controls.list` and `ctx.storage.get` work only during `activate`; handlers use their
scope. A scope closes when its handler settles. After a preset change, calls with an older scope fail
with `staleContext`. There is no module loader: bundle everything into one file (`tetrad-ext
build` does this).

### Add a tab to the Remote page

An extension can put its own page on the [Remote](remote.md#extension-tabs) page of the unit: a
tab with HTML that you write, and a handler that answers it.

1. Declare it in the manifest (API 6): the `remote` capability and a title of at most 20
   characters.

   ```json
   { "api": 6, "capabilities": ["remote"], "remote": { "title": "Notes" } }
   ```

2. Write the page as `remote.html` next to `tetrad-extension.json`. `tetrad-ext package` puts it
   in the package (format 3; at most 256 KiB). It is a piece of HTML with its own `<style>` and
   `<script>`, not a whole document (Remote adds the doctype and the bridge below).
3. Answer its messages in `activate`:

   ```ts
   ctx.remote.onMessage(async (message, scope) => {
     return { echo: message }; // any JSON; a throw replies with an error
   });
   ```

   The handler runs as every handler does: the same limits, and the same capabilities (for
   example `storage`). A message is JSON of at most 64 KiB. One handler per extension, and it
   works while the extension runs: a stopped extension answers with an error.
4. In the page, `tetrad.send(message)` returns a promise with what the handler returned, or
   rejects with its error text. That is the only way out of the page: it runs in a frame without
   access to the Remote page, its PIN, its storage or the network (the frame is sandboxed and its
   content policy has `connect-src 'none'`). The page can use inline styles and scripts and
   `data:` images, nothing else.

`extensions/fixtures/remote-tab` is a complete example (a note that the unit keeps). A call that
takes longer than 5 seconds gets "The unit did not answer" on the page, and the handler's answer
is dropped. Keep the handler quick, and for slow work answer at once and let the page ask again.

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

A manifest with themes (or a footswitch mode) and no capabilities, settings or commands is
**data only**:

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

### Ship a program

A package can bring a native program, such as a game, and start it from the extension's page.
The program runs as Tetrad does: as root, with full access to the unit, outside the limits of
the TypeScript extensions. Tetrad shows **Runs a program with full access to the unit** on the
Available row, before the install.

While the program runs:

- Tetrad draws nothing on the screen; the program can write `/dev/fb0` (800×480, RGB565).
- Tetrad does not act on the footswitches, the knobs and TEMPO. The program can read them from
  `/dev/zencoder/knob_stomp1` to `knob_stomp11` (evdev): each reader gets every event.
- The sound, MIDI, the volume knob and the expression pedals stay with Tetrad.
- A touch held for 2 seconds, or the power button, stops the program (SIGKILL). The program
  should not need the touch screen. When it ends, Tetrad draws the screen again.

Declare it in the manifest (API 7) and put its files in `app/`, next to the manifest:

```json
{
  "api": 7,
  "app": { "label": "Play DOOM", "exec": "qcdoom", "args": ["-iwad", "{app}/doom1.wad"] }
}
```

- `label` is the button on the extension's page (at most 32 characters).
- `exec` is the file in `app/` that runs: a 32-bit ARM Linux program. A static binary is the
  safest; the unit runs Linux 4.0.
- `args` are at most 16 arguments. `{app}` in an argument becomes the folder of the program
  files. The program runs in the extension's asset folder (`/media/p4/tetrad/assets/<id>/`),
  where it can write its saves.

`tetrad-ext package` pins each file of `app/` in the package by size and SHA-256 (format 4) and
copies it next to the package as `<package name without .tetradext>.<file>`. Publish these files
as assets of the same release as the package. Tetrad downloads them at the install and refuses
a file that differs. A file can be up to 32 MiB, and all files together up to 64 MiB. A package
with only a program needs no module. `extensions/doom` is a complete example.

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
