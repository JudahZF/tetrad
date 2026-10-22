# Extensions

Extensions add small programs to Tetrad. You install them on the unit from a repository on
GitHub. An extension is written in TypeScript and runs in its own process, so a slow or broken
extension cannot stop your sound or the touch UI.

The first extension is **MIDI Mapper**: it reserves a second MIDI channel and offers it as a
trigger on the [Mappings](mappings.md) page.

- [Use extensions](#use-extensions)
- [Write an extension](#write-an-extension)
- [Publish a repository](#publish-a-repository)

## Use extensions

Open **Settings → System → Extensions** (the last row).

### Install

1. Tap **REFRESH**. Tetrad reads the catalog of every trusted repository.
2. Under **Available**, tap an extension, then **INSTALL**.

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

Each extension runs in a separate low-priority process with a 128 MiB address-space limit, an
8 MiB JavaScript heap, 50 ms of CPU time for each piece of work, 1 s for an event handler and
3 s to start. Tetrad stops an extension that goes past a limit.

## Write an extension

Requirements: Node.js with npm, and the `tetrad-ext` command
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
  "api": 1,
  "capabilities": ["midiReceive", "triggers"],
  "settings": [
    { "key": "channel", "label": "MIDI channel", "kind": "choice", "default": 1,
      "options": [{ "value": 1, "label": "Channel 1" }, { "value": 2, "label": "Channel 2" }] }
  ],
  "commands": [{ "id": "reset", "label": "Reset" }]
}
```

Capabilities: `midiReceive`, `midiSend`, `controlsRead`, `controlsEdit`, `storage` and
`triggers`. The host refuses every call that needs a capability the manifest does not list.
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
