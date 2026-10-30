# Tetrad Remote HTTP API: typed contract

Source: PR #42 at its head `950e704`; `crates/tetrad-remote` is the same on `main` at its merge
`3a3b247`. Every citation is `path:line` at `950e704`. Dev-only routes (`/api/update*`, `/api/auth/*`, `/api/keys`,
`/api/log`) are left out.

Abbreviations: `http.rs` = `crates/tetrad-remote/src/http.rs`, `api.rs` =
`crates/tetrad-remote/src/api.rs`, `dev/` = `crates/tetrad-app/src/device/`, `ui/` =
`crates/tetrad-ui/src/`, `parts/` = `crates/tetrad-remote/src/parts/`.

## 1. Transport rules

| Rule | Value | Source |
| --- | --- | --- |
| Port | 80, plain HTTP | `docs/remote.md:345` |
| PIN header | `x-tetrad-pin` (case-insensitive name). Value: the 4-digit PIN, trimmed. An empty value counts as no PIN. | `api.rs:5`, `http.rs:502-504` |
| Page sends it as | `X-Tetrad-Pin: <pin>` on every call | `crates/tetrad-remote/src/page.html:151` |
| `Host` header | Must be an IPv4 literal, optional `:port`. Otherwise **421** `open the page by the unit's IP address`. A hostname (mDNS, DNS) is refused. | `http.rs:120-122`, `http.rs:517-522` |
| One request per connection | The reply has `Connection: close`. No keep-alive. | `http.rs:578-581` |
| Request body | Needs `Content-Length`. A POST without it gets **411**. `Transfer-Encoding` (chunked) gets **411**. Max body 65,536 bytes (**413**). | `http.rs:13`, `http.rs:465-475` |
| Empty POST | The page sends `body: ""` so that `Content-Length: 0` goes out. Do the same in React Native. | `page.html:164-168` |
| Max head | 8 KiB (**431**), 32 headers (**431**) | `http.rs:10-11`, `http.rs:451-461` |
| Query decoding | `%XX` and `+` = space; bad escape or bad UTF-8 = **400** `malformed query`. First match of a key wins. | `http.rs:525-550` |
| Request `Content-Type` | Never checked. The page sets `audio/wav`, `application/octet-stream` or `application/json` only for clarity. | `http.rs` (no check) |
| Timeouts | 10 s per connection (send request + read reply). 5 s for the device to answer, then **503** `The unit did not answer`. | `crates/tetrad-remote/src/server.rs:20-22`, `api.rs:435` |
| Wrong PIN | Reply after a 1 s delay. After 10 wrong PINs in a row the unit makes a new PIN. | `server.rs:24`, `server.rs:147-153`, `dev/remote.rs:33`, `dev/remote.rs:236-242` |
| CORS | No preflight answer (OPTIONS gets 404/405). Not a problem for a native app. | `api.rs:3-4` |
| Response headers | `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` | `http.rs:578-582` |
| QR code login | The unit's QR opens `http://<ip>/#pin=NNNN`. The page reads `^#pin=([0-9]{4})$`. | `page.html:525-533` |

## 2. Errors

Every error body is JSON:

```ts
/** Every non-200 reply, and two 200 replies (see TabCallReply). */
export interface ApiError {
  error: string;
}
```

| Status | `error` text | Who makes it | Source |
| --- | --- | --- | --- |
| 400 | `incomplete request`, `malformed request`, `malformed PIN`, `malformed Content-Length`, `incomplete request body`, `malformed query`, `name required`, `id required`, `offset required`, `folder required` | server | `http.rs:118-550` |
| 401 | `PIN required` (no header, or empty) | server | `http.rs:123` |
| 401 | `Wrong PIN` | device | `api.rs:432` |
| 404 | `not found` | server | `http.rs:346`, `:394`, `:423` |
| 405 | `method not allowed` | server | `http.rs:322-345`, `:385-393`, `:415-422` |
| 411 | `chunked bodies are not supported`, `Content-Length required` | server | `http.rs:465-473` |
| 413 | `request body too large` | server | `http.rs:469-470` |
| 421 | `open the page by the unit's IP address` | server | `http.rs:121` |
| 422 | Any refusal reason from the device (`Reply::Refused`), human-readable English. Show it as is. | device | `api.rs:434` |
| 431 | `request header too large`, `too many headers` | server | `http.rs:451-460` |
| 503 | `Remote is off` (Remote stopped on the unit) | device / server | `api.rs:433` |
| 503 | `The unit did not answer` (5 s timeout) | server | `api.rs:435` |

Check order (matters for error handling):

- Most routes: PIN presence (401) first, then query params (400). `http.rs:126-321`.
- **`/api/library/*` and `/api/backup/*`: query params, path and method first (400/404/405), PIN
  after.** A call with no PIN and a bad query gets 400, not 401. `http.rs:264-277`.
- The device checks the PIN value (401 `Wrong PIN`) before anything else. `dev/remote.rs:232-243`.
- A 401 means the PIN changed (Remote restarted, or 10 wrong PINs). The page forgets the PIN and
  asks again. `page.html:159-160`.

## 3. Route table

All routes need the PIN header. "Reply" is the 200 body.

| Method | Path | Query | Body | 200 reply | Handler |
| --- | --- | --- | --- | --- | --- |
| GET | `/api/status` | – | – | `StatusReply` (422 if the library is closed; the PIN is still right) | `dev/remote.rs:245-248` |
| POST | `/api/irs` | `name` (required, not blank) | IR WAV bytes (see §5) | `IrAddedReply` | `dev/remote.rs:665-696` |
| GET | `/api/library` | – | – | `IrLibrary` | `dev/remote_library.rs:42-86` |
| POST | `/api/library/rename` | `id`, `name` | empty | `IrLibrary` | `dev/remote_library.rs:88-98` |
| POST | `/api/library/delete` | `id` | empty | `IrLibrary` | `dev/remote_library.rs:101-109` |
| POST | `/api/library/link` | `id`, `folder` (i64), `linked` (`1` = in, else out) | empty | `IrLibrary` | `dev/remote_library.rs:113-125` |
| POST | `/api/library/destination` | `folder` (i64) | empty | `IrLibrary` | `dev/remote_library.rs:28-30` |
| POST | `/api/library/folder` | `name`, `parent` (optional i64; absent = IR root) | empty | `IrLibrary` | `dev/remote_library.rs:128-146` |
| GET | `/api/files` | – | – | `FilesReply` | `api.rs:394-409`, `dev/remote_files.rs:56-98` |
| POST | `/api/files/chunk` | `id`, `name`, `offset` (u64), `last` (`1` = last) | raw bytes of the piece | `FileReceivedReply` or `FileStoredReply` | `dev/remote_files.rs:103-166` |
| POST | `/api/files/delete` | `id`, `name` | empty | `FileDeletedReply` | `dev/remote_files.rs:169-188` |
| GET | `/api/tabs` | – | – | `TabsReply` | `dev/remote.rs:588-599` |
| GET | `/api/tabs/page` | `id` | – | `TabPageReply` | `dev/remote.rs:601-610` |
| POST | `/api/tabs/call` | `id` | JSON message (any JSON value) | `TabCallReply` | `dev/remote.rs:614-646`, `api.rs:421-428` |
| GET | `/api/support` | – | – | **`application/zip`** bytes, `Content-Disposition: attachment; filename="<name>"` | `dev/remote.rs:260-263`, `http.rs:86-91` |
| GET | `/api/notices` | – | – | `NoticesReply` | `dev/remote.rs:475-494` |
| GET | `/api/backup` | – | – | `BackupStatus` | `dev/remote_backup.rs:93-110` |
| POST | `/api/backup/create` | – | empty | `BackupStatus` | `dev/remote_backup.rs:143-160` |
| GET | `/api/backup/file` | `offset` (u64) | – | **`application/octet-stream`**, at most 524,288 bytes from `offset` | `dev/remote_backup.rs:163-181`, `http.rs:92` |
| POST | `/api/backup/upload` | `name`, `offset` (u64), `last` (`1` = last) | raw bytes of the piece | `BackupStatus` | `dev/remote_backup.rs:186-264` |
| POST | `/api/backup/discard` | – | empty | `BackupStatus` | `dev/remote_backup.rs:267-274` |
| GET | `/api/extensions` | – | – | `ExtensionsList` | `dev/remote_extensions.rs:174-259` |
| POST | `/api/extensions/command` | – | JSON `ExtensionCommand` | `ExtensionsList` | `dev/remote_extensions.rs:20-89` |
| GET | `/api/themes` | – | – | `ThemesList` | `dev/remote_themes.rs:124-174` |
| POST | `/api/themes/save` | – | JSON `ThemeFile` | `ThemesList` | `dev/remote_themes.rs:178-220` |
| POST | `/api/themes/select` | `id` (theme id) | empty | `ThemesList` | `dev/remote_themes.rs:224-230` |
| POST | `/api/themes/delete` | `id` (must be `file:<id>`) | empty | `ThemesList` | `dev/remote_themes.rs:234-263` |
| GET | `/api/mappings` | – | – | `MappingsView` | `dev/remote_mappings.rs:395-407` |
| POST | `/api/mappings/command` | – | JSON `MappingCommand` | `MappingsView` | `dev/remote_mappings.rs:105-139` |
| GET | `/api/footswitch` | – | – | `FootswitchView` | `dev/remote_footswitch.rs:142-159`, `:237-289` |
| POST | `/api/footswitch/command` | – | JSON `FootswitchCommand` | `FootswitchView` | `dev/remote_footswitch.rs:142-207` |
| GET | `/` or `/index.html` | – | – | the HTML page (no PIN) | `http.rs:125` |

Routing source: `http.rs:118-348` (main), `http.rs:350-396` (library), `http.rs:398-425` (backup).
Query helpers: `id` must be non-empty (`http.rs:427-431`); `name` must be non-empty
(`http.rs:433-437`), except `/api/irs` and library `folder`/`parent`, which must also be non-blank
after trim (`http.rs:131-133`, `http.rs:351-360`). Boolean flags are true only for the exact
string `"1"` (`http.rs:237`, `:373`, `:411`).

Every write reply is the full new state (library listing, backup status, extensions list, themes
list, mappings view, footswitch view). Redraw from it; no second GET is needed.

## 4. Types

```ts
// ---------- status / IRs ----------

/** GET /api/status. dev/remote.rs:245-248, api.rs:365 */
export interface StatusReply {
  /** Name of the IR saving destination folder ("IRs" when unnamed). dev/remote.rs:739-750 */
  folder: string;
}

/** POST /api/irs. api.rs:366, dev/remote.rs:685 */
export interface IrAddedReply {
  /** The stored name: whitespace runs collapsed to one space, trimmed. */
  name: string;
  /** The folder it went to. */
  folder: string;
}

/** Every /api/library reply. dev/remote_library.rs:42-86 */
export interface IrLibrary {
  /** Folder id of the saving destination. */
  destination: number;
  /** Writable IR folders, the root first (dev/remote_library.rs:192-203). */
  folders: IrFolder[];
  /** User IRs only (factory IRs are not listed), in zenui.db order. */
  irs: IrEntry[];
}
export interface IrFolder {
  id: number;
  /** "IRs" when the stored name is empty (dev/remote_library.rs:206-211). */
  name: string;
  /** Parent folder id; 0 for the root. */
  parent: number;
  /** parent === 0. Every IR is always in the root; unlinking from it is refused. */
  root: boolean;
}
export interface IrEntry {
  /** Product id (opaque string). */
  id: string;
  name: string;
  /** Text as stored. IRs that Remote imports get "YYYY-MM-DDTHH:MM:SSZ"
   *  (crates/tetrad-library/src/sidecar.rs:211-235); older IRs keep what stock wrote. */
  date: string;
  /** Ids of the folders that hold it (the root included). */
  folders: number[];
}

// ---------- extension files (e.g. NAM models) ----------

/** GET /api/files. api.rs:394-409, api.rs:336-357 */
export interface FilesReply {
  extensions: ExtensionFiles[];
}
export interface ExtensionFiles {
  /** Extension id (not the install key), e.g. "tetrad.nam". Use as `id=`. */
  id: string;
  name: string;
  /** What the files are, e.g. "NAM model". */
  label: string;
  /** Allowed file extensions with the dot, lower case, e.g. [".nam"]. */
  extensions: string[];
  /** Max file size: 3,145,728 (dev/extension_jobs.rs:22, dev/remote_files.rs:20). */
  maxBytes: number;
  files: ExtensionFile[];
}
export interface ExtensionFile {
  name: string;
  size: number;
  /** dev/extensions.rs:633-648 */
  state: "ready" | "waiting" | "failed";
  /** Why it failed; "" otherwise. */
  detail: string;
}
/** POST /api/files/chunk without last=1. api.rs:410 */
export interface FileReceivedReply { received: number }
/** POST /api/files/chunk with last=1. api.rs:411 */
export interface FileStoredReply { name: string; size: number }
/** POST /api/files/delete. api.rs:412 */
export interface FileDeletedReply { name: string }

// ---------- extension tabs ----------

/** GET /api/tabs. api.rs:413-419 */
export interface TabsReply {
  tabs: { id: string; title: string }[];
}
/** GET /api/tabs/page. api.rs:420 */
export interface TabPageReply {
  /** Extension HTML. The web page puts it in a sandboxed iframe after a bridge script
   *  that defines `tetrad.send(message)` (page.html:431-452). */
  html: string;
}
/** POST /api/tabs/call. Status 200 in BOTH cases (api.rs:421-428). */
export type TabCallReply =
  | { result: unknown }
  /** The extension's answer was not JSON. */
  | { error: "The extension answered with bad JSON" };

// ---------- notices ----------

/** GET /api/notices. dev/remote.rs:475-494 */
export interface NoticesReply {
  /** Newest last. Private values are taken out, as in the support file. */
  notices: Notice[];
  /** Older notices no longer kept (dev/mod.rs:563). */
  dropped: number;
  /** Times the last notice repeated (dev/mod.rs:565). */
  repeats: number;
}
export interface Notice {
  /** Seconds before the reply. The page computes `Date.now() - ago * 1000`. */
  ago: number;
  /** Rust Debug name of dev/mod.rs:533-550. */
  subsystem: Subsystem;
  detail: string;
}
export type Subsystem =
  | "Platform" | "Splash" | "Settings" | "Library" | "Dsp" | "Preset" | "Ui" | "Control"
  | "Input" | "Midi" | "Services" | "Capture" | "Power" | "Extensions" | "Remote" | "Usb";

// ---------- backup ----------

/** Every /api/backup reply except /file. dev/remote_backup.rs:93-110 */
export interface BackupStatus {
  state: BackupState;
  /** Set for ready, receiving, received, restoring; else null. */
  name: string | null;
  /** Set for ready, received, restoring; else null. */
  size: number | null;
  /** Set for receiving; else null. */
  received: number | null;
  /** The last problem (pack or restore failed, archive gone); else null. */
  error: string | null;
}
/** dev/remote_backup.rs:32-57, :94-102 */
export type BackupState =
  | "idle"
  | "creating"   // the pack job runs: poll GET /api/backup (page: every 1 s)
  | "ready"      // packed; download with GET /api/backup/file
  | "receiving"  // an upload is in progress
  | "received"   // upload done; waits for RESTORE or DISCARD touched on the unit
  | "restoring"
  | "restored";
```

Backup rules (`dev/remote_backup.rs`):

- Packed name: `QC-Backup-YYYYMMDD-HHMM.tar.gz` (UTC) or `QC-Backup.tar.gz` (`:412-423`).
- Download: loop `GET /api/backup/file?offset=N` until `N == size`; each piece ≤ 524,288 bytes
  (`:23`). `offset > size` = 422. Then the page calls `POST /api/backup/discard`: the unit keeps
  no copy (`parts/backup.html:91-117`).
- Upload: pieces at `offset` 0, then exactly where the stored part ends (422 `Out of order: ...`
  otherwise). `offset=0` drops any earlier backup. `name` must stay the same for all pieces.
  Control chars are removed and it is cut to 64 chars (`:194-197`). Max 536,870,912 bytes
  (`:25`). The last piece must make a gzip file (magic `1f 8b`), else 422 (`:243-250`). The page
  uses 48 KiB pieces (`parts/backup.html:39`) because the body limit is 64 KiB.
- `create` is refused while `received` waits (`:145-149`); `create`/`upload`/`discard` are refused
  while `creating` or `restoring` (`:83-89`).
- The restore itself starts only by a touch on the unit.

```ts
// ---------- extensions ----------

/** GET /api/extensions and every command reply. dev/remote_extensions.rs:174-205 */
export interface ExtensionsList {
  installed: InstalledExtension[];
  available: AvailableExtension[];
  /** Read only: repositories are added and trusted on the unit only. */
  sources: ExtensionSource[];
  /** Running job or last result, e.g. "Installing MIDI Mapper…" (ui/extensions.rs:14-15). */
  status: string | null;
  /** A repository job runs (refresh, install, update, trust). */
  busy: boolean;
}

/** dev/remote_extensions.rs:207-259, ui/extensions.rs:21-51 */
export interface InstalledExtension {
  /** Install key "owner/repo:extension.id". Use it in commands. */
  key: string;
  name: string;
  version: string;
  description: string;
  /** "owner/repo". */
  source: string;
  enabled: boolean;
  /** dev/remote_extensions.rs:208-213 */
  state: "off" | "starting" | "running" | "faulted";
  /** The fault reason when state === "faulted"; else null. */
  fault: string | null;
  /** Newer version in the catalog; else null. */
  update: string | null;
  /** Other repos ("Default" or "owner/repo") whose copy of this extension is on too. */
  duplicates: string[];
  commands: { id: string; label: string }[];
  settings: ExtensionSettingView[];
  /** Lines set with scope.ui.setStatus. */
  status: string[];
  blocks: { name: string; detail: string; ready: boolean }[];
  /** Newest log lines, oldest first. */
  log: string[];
  /** Theme names it adds. */
  themes: string[];
  /** Footswitch mode it adds; else null. (`provides` of ui/extensions.rs:61 is not sent.) */
  mode: { name: string; detail: string } | null;
  /** Button label of its program, e.g. "Play DOOM"; else null. */
  app: string | null;
}

/** dev/remote_extensions.rs:214-231 */
export type ExtensionSettingView =
  | { key: string; label: string; kind: "choice"; options: string[]; /** selected index */ value: number }
  | { key: string; label: string; kind: "toggle"; value: boolean }
  | { key: string; label: string; kind: "number"; value: number; min: number; max: number };

/** dev/remote_extensions.rs:175-187, ui/extensions.rs:113-125 */
export interface AvailableExtension {
  source: string;   // "owner/repo"
  id: string;       // extension id inside the repo
  name: string;
  version: string;
  description: string;
  duplicates: string[];
  /** It brings a program with full access to the unit: the page asks for consent first. */
  app: boolean;
}

/** dev/remote_extensions.rs:188-197. `fingerprint` and `pending_trust` are not sent. */
export interface ExtensionSource {
  repository: string;
  default: boolean;
  enabled: boolean;
  /** "Up to date", "Not refreshed", "Refresh failed: …". */
  status: string;
}

/** POST /api/extensions/command body. serde `tag = "op"`, lowercase.
 *  dev/remote_extensions.rs:20-56. Checks: dev/remote_extensions.rs:94-171. */
export type ExtensionCommand =
  | { op: "refresh" }
  /** Must be in `available` (422 "<source> does not offer <id>; refresh the list"). */
  | { op: "install"; source: string; id: string }
  /** Needs `update !== null`. */
  | { op: "update"; key: string }
  | { op: "enable"; key: string }
  | { op: "disable"; key: string }
  /** Needs state "faulted". */
  | { op: "retry"; key: string }
  /** Settings and stored data stay. The page asks first. */
  | { op: "remove"; key: string }
  /** `command` = an id from `commands`. */
  | { op: "run"; key: string; command: string }
  /** `value`: choice = option index (non-negative integer < options.length);
   *  toggle = boolean; number = integer in [min, max]. */
  | { op: "setting"; key: string; setting: string; value: number | boolean };

// ---------- themes ----------

/** "#rrggbb", lower case on output (crates/tetrad-theme/src/lib.rs:42-64). */
export type Hex = string;

/** crates/tetrad-theme/src/lib.rs:70-139 (camelCase serde). */
export type PaletteRole =
  | "background" | "canvas" | "panel" | "surface" | "rowSelected" | "section" | "divider"
  | "scrollbar" | "text" | "muted" | "faint" | "cable" | "accent" | "accentFill" | "onAccent"
  | "action" | "danger" | "warning";
export type Palette = Record<PaletteRole, Hex>;
export type Colors = Partial<Record<PaletteRole, Hex>>;

/** Theme id text (crates/tetrad-theme/src/spec.rs:233-292):
 *  built-in "stock" | "stage-contrast" | "slate" | "ember" (crates/tetrad-theme/src/builtin.rs:15-90),
 *  `file:<id>`, or `ext:<owner/repo:extension.id>/<id>`. */
export type ThemeId = string;

/** GET /api/themes and every op reply. dev/remote_themes.rs:124-174 */
export interface ThemesList {
  /** The selected theme id; null if none is marked selected. */
  active: ThemeId | null;
  /** The editor's role list, fixed order (dev/remote_themes.rs:20-89). */
  fields: { key: PaletteRole; label: string; help: string; /** resolve derives it when absent */ derived: boolean }[];
  /** Built-ins first, then files, then extension themes (ui/view.rs:2192-2193). */
  themes: ThemeEntry[];
}
export interface ThemeEntry {
  id: ThemeId;
  name: string;
  source: "builtin" | "file" | "extension";
  /** "Built-in", "Theme file" or the extension's name (ui/view.rs:2201-2202). */
  origin: string;
  /** Why it failed its checks; null when it is good. */
  error: string | null;
  /** Resolved colours; null when `error` is set. */
  palette: Palette | null;
  /** Base of a file/extension theme; null for built-ins or when unset. */
  base: string | null;
  /** Colours the theme sets. Built-ins: {}. null: a theme file that does not parse. */
  colors: Colors | null;
}

/** POST /api/themes/save body (crates/tetrad-theme/src/spec.rs:96-127, deny_unknown_fields).
 *  Max 64 KiB (dev/appearance.rs:18); max 32 theme files (dev/appearance.rs:20). */
export interface ThemeFile {
  version: 1;
  /** 1..=32 chars of [a-z0-9-], not starting with "-" (spec.rs:9, :56). Not a built-in id.
   *  Saved as <id>.json; saving an existing id replaces its file. */
  id: string;
  /** 1..=24 chars, no control chars (spec.rs:11, :58). */
  name: string;
  /** A built-in id; "stock" when absent. */
  base?: string;
  /** Only the roles it sets. Must pass the dark-only and contrast checks of `resolve`. */
  colors: Colors;
}
```

`/api/themes/delete` takes `id=file:<id>`; anything else is 422 `only theme files can be
deleted`. Deleting the active theme selects Stock (`dev/remote_themes.rs:234-263`).

Mappings and footswitch types are in §6 and §7.

## 5. IR conversion

What the web page does, from `parts/irs.html:37-115`, and what the unit checks
(`crates/tetrad-media/src/ir.rs:442-597`).

### Pipeline

| Step | Page behaviour | Notes for a TS reimplementation |
| --- | --- | --- |
| Decode | `new OfflineAudioContext(1, 1, 48000).decodeAudioData(bytes)`. Any format the browser decodes (the picker offers `.wav,.wave,.aif,.aiff`). Failure: "Not an audio file". | You need a WAV/AIFF decoder (PCM 16/24/32-bit int, 32/64-bit float). |
| Resample | `decodeAudioData` resamples to the context rate, 48,000 Hz. The algorithm is browser-defined. | Use any good resampler (e.g. windowed sinc). Skip it when the file is already 48 kHz. |
| Channel mix | Render through a 1-channel `OfflineAudioContext` destination (`channelInterpretation` default `"speakers"`). Mono = as is. Stereo = `0.5 * (L + R)`. | Web Audio spec down-mix to mono: 4 ch (quad) = `0.25 * (L + R + SL + SR)`; 6 ch (5.1) = `0.7071 * (L + R) + C + 0.5 * (SL + SR)`; other counts are "discrete": only channel 0. The page labels >2 channels "N channels → mono". |
| Length | `frames = Math.min(1024, decoded.length)` at 48 kHz: the first 1,024 samples (21.3 ms). Longer files are cut, shorter files are not padded. | No fade, no window, no normalisation, no gain, no clamp. |
| Format | IEEE float32, little-endian, mono. | `Float32Array` of the samples. |
| Header | 46-byte header below, data chunk header at byte 38. | Total bytes = `46 + 4 * n` (4,142 for 1,024 samples). |
| Name | File name without its last extension, whitespace runs to one space, trimmed, cut to 60 UTF-16 units (no split surrogate). Empty = not sent. | Then `encodeURIComponent` into `?name=`. |
| Send | `POST /api/irs?name=<name>`, `Content-Type: audio/wav`, body = WAV. One at a time (shared queue). | Reply 200 `IrAddedReply`, or 422 with the reason. |

Quoted JS (`parts/irs.html:37-39`, `:43-51`, `:57-70`, `:73-92`, `:105-109`):

```js
const RATE = 48000;
const SAMPLES = 1024;
const MAX_NAME = 60;

/** The file name without its extension, as the unit shows names: at most 60 UTF-16 units. */
const irName = (file) => {
  let name = file.name.replace(/\.[^.]*$/, "").replace(/\s+/g, " ").trim();
  if (name.length > MAX_NAME) {
    name = name.slice(0, MAX_NAME);
    const last = name.charCodeAt(name.length - 1);
    if (last >= 0xd800 && last <= 0xdbff) name = name.slice(0, -1);
  }
  return name;
};

/**
 * Any file the browser decodes → 48 kHz (decodeAudioData resamples to its context rate) →
 * one channel (the destination mixes stereo to ½ L + ½ R) → the first 1,024 samples.
 */
const convert = async (file) => {
  const decoded = await new OfflineAudioContext(1, 1, RATE).decodeAudioData(await file.arrayBuffer());
  const frames = Math.min(SAMPLES, decoded.length);
  const context = new OfflineAudioContext(1, frames, RATE);
  const source = context.createBufferSource();
  source.buffer = decoded;
  source.connect(context.destination);
  source.start();
  const rendered = await context.startRendering();
  const count = decoded.numberOfChannels;
  const channels = count === 1 ? "mono" : (count === 2 ? "stereo" : count + " channels") + " → mono";
  const cut = decoded.length > SAMPLES ? ", first " + SAMPLES + " samples" : "";
  return { samples: rendered.getChannelData(0), detail: "48 kHz " + channels + cut };
};

/** The stock IR WAV: RIFF, an 18-byte IEEE-float fmt chunk, the data chunk at byte 38. */
const wav = (samples) => {
  const view = new DataView(new ArrayBuffer(46 + samples.length * 4));
  const tag = (at, text) => { for (let i = 0; i < 4; i++) view.setUint8(at + i, text.charCodeAt(i)); };
  tag(0, "RIFF");
  view.setUint32(4, 38 + samples.length * 4, true);
  tag(8, "WAVE");
  tag(12, "fmt ");
  view.setUint32(16, 18, true);
  view.setUint16(20, 3, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, RATE, true);
  view.setUint32(28, RATE * 4, true);
  view.setUint16(32, 4, true);
  view.setUint16(34, 32, true);
  view.setUint16(36, 0, true);
  tag(38, "data");
  view.setUint32(42, samples.length * 4, true);
  samples.forEach((sample, i) => view.setFloat32(46 + i * 4, sample, true));
  return view.buffer;
};

// send():
const { status, body } = await call("/api/irs?name=" + encodeURIComponent(name), {
  method: "POST",
  headers: { "Content-Type": "audio/wav" },
  body: wav(converted.samples),
});
```

### WAV header layout (all little-endian)

| Offset | Size | Value | Unit check (`ir.rs`) |
| --- | --- | --- | --- |
| 0 | 4 | `"RIFF"` | must match (`:495-499`) |
| 4 | u32 | `38 + 4n` (file length − 8) | must equal file length − 8 exactly (`:503-510`) |
| 8 | 4 | `"WAVE"` | must match (`:500-502`) |
| 12 | 4 | `"fmt "` | must be here (`:523`) |
| 16 | u32 | 18 | must be 18 (`:523`) |
| 20 | u16 | 3 (IEEE float) | must be 3 (`:447`) |
| 22 | u16 | 1 channel | must be 1 (`:448`) |
| 24 | u32 | 48000 | must be 48000 (`:449`) |
| 28 | u32 | 192000 (byte rate) | not checked in this policy |
| 32 | u16 | 4 (block align) | not checked in this policy |
| 34 | u16 | 32 bits | must be 32 (`:450`) |
| 36 | u16 | 0 (cbSize) | not checked in this policy |
| 38 | 4 | `"data"` | chunks are read from 38 on, no odd padding (`:533-590`) |
| 42 | u32 | `4n` | must fit in the file |
| 46 | 4n | float32 samples | the unit keeps at most 1,024; partial words dropped (`:416-428`) |

### Name rules on the unit

- The unit collapses whitespace (`split_whitespace().join(" ")`,
  `crates/tetrad-media/src/user.rs:194-196`) and needs **1 to 60** UTF-16 units for an upload
  (`dev/remote.rs:665-672`), 422 `An IR name has 1 to 60 characters`.
- **Rename and new folder need 4 to 60** (`crates/tetrad-media/src/user.rs:31-32`, `:200-208`;
  `dev/remote_library.rs:89`, `:129`, `:213-215`). So a 1–3 character upload name works, but the
  IR cannot be renamed to a name that short.
- An IR with the same samples as one in the library replaces it (`docs/remote.md:337-338`).

## 6. Mappings model

### The view

```ts
/** GET /api/mappings and every command reply. dev/remote_mappings.rs:395-407 */
export interface MappingsView {
  /** Loaded preset name ("" when none). */
  preset: string;
  rows: PerScope<MappingRow[]>;
  /** The Type list per scope. dev/remote_mappings.rs:411-429 */
  triggers: PerScope<TriggerChoice[]>;
  /** The action menu per scope. dev/remote_mappings.rs:431-441 */
  actions: PerScope<ActionGroup[]>;
  /** The row the last command added or changed. Follow it after "add". */
  focus: RowRef | null;
  /** The row waiting for LEARN. The page polls GET every 1 s while it is set. */
  learning: RowRef | null;
  /** Last refusal; cleared by the next accepted command. */
  notice: string | null;
}
export type MappingScope = "preset" | "global";
export interface PerScope<T> { preset: T; global: T }
export interface RowRef { scope: MappingScope; id: string }

/** dev/remote_mappings.rs:443-486 */
export interface MappingRow {
  /** OPAQUE: Rust Debug text of ui/mappings.rs:28-41 (see below). Send it back unchanged.
   *  It can change after an edit (the row moves between stock and Tetrad storage): use `focus`. */
  id: string;
  trigger: MappingTriggerView;
  /** "Stomp A", "CC 20", "Preset loaded", "Ch 3 CC 21". */
  triggerLabel: string;
  /** "Footswitch press", "MIDI · QC channel", "MIDI Mapper · Ch 3". */
  triggerDetail: string;
  /** true for cc, note and extension triggers. */
  learns: boolean;
  /** OPAQUE action key (Rust Debug of MappingAction); null = no action yet. */
  action: string | null;
  /** "" without an action. */
  actionLabel: string;
  actionDetail: string;
  /** action === "SendMidi". Its messages are edited on the unit only. */
  sendMidi: boolean;
  /** null without an action. */
  mode: ModeView | null;
  /** Modes the row accepts, menu order. Empty = fixed mode. */
  modes: ModeView[];
  /** CC window (low/high) first, then the mode's values. */
  fields: ValueField[];
  /** One-line values, e.g. "25 % ↔ 60 % · CC 64–127"; "" when nothing. */
  summary: string;
  /** CC (and extension CC) triggers only, not Relative. */
  response: ResponseView | null;
}

/** dev/remote_mappings.rs:444-455 */
export type MappingTriggerView =
  | { kind: "presetLoaded" }
  /** 0..=7 = A..=H */
  | { kind: "stomp"; stomp: number }
  /** 1 or 2 */
  | { kind: "expression"; pedal: number }
  /** 0..=127 */
  | { kind: "cc"; number: number }
  | { kind: "note"; number: number }
  /** `source` = what fired it, e.g. "cc:20"; "" = waits for LEARN. */
  | { kind: "extension"; key: string; trigger: string; source: string };

/** dev/remote_mappings.rs:359-369, ui/mappings.rs:173-190 */
export type ModeKind = "onPress" | "follow" | "relative" | "set" | "toggle" | "momentary" | "step";
export interface ModeView { kind: ModeKind; /** "Toggle" */ label: string; /** "Each press swaps A and B" */ detail: string }

/** dev/remote_mappings.rs:371-385, ui/mappings.rs:203-224 */
export type FieldKey =
  | "value"     // Set, Momentary
  | "a" | "b"   // Toggle
  | "amount"    // Step: steps per press, signed
  | "wrap"      // Step: stop or wrap
  | "min" | "max" // Follow: value at window Low / High
  | "pickup"    // Follow: Jump or Pickup
  | "encoding"  // Relative: encoder coding
  | "low" | "high"; // CC window 0..=127
export interface ValueField { id: FieldKey; /** "A", "Low" */ label: string; /** "25 %", "Plate" */ text: string }

/** CC across 0..=127; target in thousandths 0..=1000. dev/remote_mappings.rs:460-469, ui/mappings.rs:238-252 */
export interface ResponseView {
  low: number;
  high: number;
  /** Follow on a parameter: target at low and at high; null = window is where it presses. */
  line: [number, number] | null;
  /** Last CC value received. */
  level: number | null;
  /** Follow on a parameter: target value now. */
  current: number | null;
  /** Pickup waits for the CC to reach `current`. */
  waiting: boolean;
}

/** dev/remote_mappings.rs:411-429 */
export type TriggerChoice =
  | { kind: "presetLoaded" | "stomp" | "expression" | "midi"; label: string }
  | { kind: "extension"; key: string; trigger: string; label: string };
// preset scope: presetLoaded "Preset loaded", stomp "Footswitch", expression "Expression pedal",
//               midi "MIDI (QC channel)", then extension triggers.
// global scope: midi only, then extension triggers.
// "midi" in this list maps to a cc or note trigger in a command.

/** dev/remote_mappings.rs:431-441 */
export interface ActionGroup {
  /** "System", the Custom mode name, "<block> · <location>", "MIDI out". */
  label: string;
  actions: { /** OPAQUE action key */ key: string; label: string }[];
}
```

Opaque ids and keys (`dev/remote_mappings.rs:6-8`): row `id` and action `key` are Rust
`format!("{:?}")` text. Treat them as opaque strings, valid while the firmware build runs; never
store them. Examples of the shape (for debugging only):

| Kind | Example |
| --- | --- |
| Row id (ui/mappings.rs:28-41) | `Tetrad(0)`, `Stomp { stomp: 0, position: GridPosition { row: 0, column: 2 } }`, `Expression { pedal: 1, position: GridPosition { row: 0, column: 2 }, target: Parameter(3) }`, `MidiOut(OnLoad)`, `MidiOut(Footswitch(A))` |
| Action key (ui/mappings.rs:106-141) | `System(NextPreset)`, `System(Scene(2))`, `System(Mode(Custom))`, `System(Looper(PlayStop))`, `System(CustomPage(Go(1)))`, `System(CustomSlot(3))`, `Block { position: GridPosition { row: 0, column: 1 }, target: Bypass }`, `Block { position: GridPosition { row: 0, column: 1 }, target: Parameter(4) }`, `SendMidi` |

Action menu content (`dev/remote_mappings.rs:286-357`), in order:

1. **System** (both scopes): Next/Previous preset, Scene A–H, Next/Previous scene, Tuner, Tap
   tempo, Preset/Scene/Stomp mode, Custom mode (only when an extension adds it), Next mode,
   Gig View, 8 looper buttons.
2. **<Custom mode name>** (both scopes, only with the mode): Next page, Previous page, Page 1..N,
   Press A..Tap (slots 0–9).
3. **<block> · <location>** per block (preset scope only): Bypass, then each parameter.
4. **MIDI out** (preset scope only): Send MIDI.

### Commands

```ts
/** POST /api/mappings/command body. serde tag "op", lowercase. dev/remote_mappings.rs:25-65.
 *  Unknown extra fields are ignored. Validation: dev/remote_mappings.rs:153-278. */
export type MappingCommand =
  /** New row with a free default trigger and no action; read its id from `focus`. */
  | { op: "add"; scope: MappingScope }
  | { op: "trigger"; scope: MappingScope; id: string; trigger: MappingTriggerInput }
  /** `action` = a `key` from actions[scope]. */
  | { op: "action"; scope: MappingScope; id: string; action: string }
  /** `mode` = a kind from the row's `modes`. */
  | { op: "mode"; scope: MappingScope; id: string; mode: ModeKind }
  /** `field` = an id from the row's `fields`; ticks in -127..=127, not 0. Page: ±1, Shift ±10. */
  | { op: "step"; scope: MappingScope; id: string; field: FieldKey; ticks: number }
  /** on: true only for cc, note, extension triggers. */
  | { op: "learn"; scope: MappingScope; id: string; on: boolean }
  | { op: "remove"; scope: MappingScope; id: string };

/** serde tag "kind", camelCase. dev/remote_mappings.rs:84-93. Numbers are u8 (0..=255 parse;
 *  the bridge refuses invalid ones). */
export type MappingTriggerInput =
  | { kind: "presetLoaded" }
  | { kind: "stomp"; stomp: number }        // 0..=7
  | { kind: "expression"; pedal: number }   // 1 | 2
  | { kind: "cc"; number: number }          // 0..=127
  | { kind: "note"; number: number }        // 0..=127
  /** Must be one of triggers[scope] kind "extension". No `source`: the same trigger keeps its
   *  learned source, another one waits for LEARN. */
  | { kind: "extension"; key: string; trigger: string };
```

Defaults the page sends when the Type changes (`parts/mappings.html:193-199`):
`presetLoaded` → `{kind:"presetLoaded"}`, `stomp` → `{kind:"stomp",stomp:0}`, `expression` →
`{kind:"expression",pedal:1}`, `midi` → `{kind:"cc",number:20}`, `extension` →
`{kind:"extension",key,trigger}`.

Command replies: a no-op (value already set) returns the view with 200
(`dev/remote_mappings.rs:116-117`). A bridge refusal with no change (e.g. a CC the QC MIDI map
uses) is 422 with the notice (`:128-135`). Bad JSON is 422 `Not a command: <serde error>`.
Unknown row: 422 `No mapping <id> in this list now; reload the page`.

Storage: global mappings save at once; preset mappings are part of the preset (save the preset
on the unit) (`docs/remote.md:94-95`).

## 7. Footswitch model (Custom mode)

### Structure

| Concept | Value | Source |
| --- | --- | --- |
| Pages | `mode.pages` = 1..=4; `page` index 0-based | `ui/custom_mode.rs:15`, `:112-115` |
| Slots per page | 10, index 0..=9 = `A B C D E F G H Down Tap` | `ui/custom_mode.rs:13`, `:16-18` |
| On-screen order | Top row A B C D Down; bottom row E F G H Tap. Page: `ORDER = [0, 1, 2, 3, 8, 4, 5, 6, 7, 9]` | `parts/footswitch.html:77-79`, `docs/remote.md:104` |
| Scopes | `"global"` = ALL PRESETS (device layout). `"preset"` = THIS PRESET (the loaded preset's own layout, saved with the preset; save the preset on the unit) | `ui/custom_mode.rs:56-63` |
| Which plays | `layouts.preset` when not null, else `layouts.global` | `ui/custom_mode.rs:141-143` |
| Colours | `colours`: 8 hex LED colours; a slot's `swatch` is an index 0..=7 or null (auto from the action) | `ui/custom_mode.rs:126-127`, `dev/custom.rs:841` |
| Label | ≤ 16 chars, no control chars; `""` = use the action's name | `dev/custom.rs:44`, `:834-835` |
| Messages | ≤ 12 per Send MIDI slot | `dev/custom.rs:46`, `:856` |
| Value | percent 0..=100, parameter actions only | `dev/remote_footswitch.rs:330-338` |
| Momentary | bypass actions only | `dev/remote_footswitch.rs:339-344` |

```ts
/** GET /api/footswitch and every command reply. dev/remote_footswitch.rs:142-159, :237-289 */
export type FootswitchView =
  /** No extension adds a Custom mode. (Commands then get 422.) */
  | { mode: null }
  | FootswitchModeView;

export interface FootswitchModeView {
  mode: {
    /** "Custom" */
    name: string;
    /** The extension's name. */
    extension: string;
    /** 1..=4 */
    pages: number;
    /** Current page, 0-based. */
    page: number;
    /** It is the footswitch mode now. */
    active: boolean;
  };
  /** Loaded preset label ("3B Clean"), for the "Load this preset" action. */
  preset: string;
  /** A preset is loaded (THIS PRESET needs one). */
  presetLoaded: boolean;
  /** 8 entries, "#rrggbb". */
  colours: string[];
  layouts: {
    /** pages × 10 slots */
    global: SlotView[][];
    /** null: the preset has no own layout. */
    preset: SlotView[][] | null;
  };
  /** SYSTEM choices of the action menu, in order. dev/remote_footswitch.rs:516-574 */
  systems: { group: SystemGroup; label: string; action: SystemActionJson }[];
  /** Blocks of the loaded preset. */
  blocks: FootswitchBlock[];
  /** Commands of running extensions. */
  commands: { key: string; command: string; /** "Control Macro · Reset" */ label: string }[];
  /** User setlist names, for "Choose a preset". */
  setlists: string[];
  /** Only in the reply to { op: "presets" }. dev/remote_footswitch.rs:155-157, :224-234 */
  presets?: { setlist: string; slots: { /** 0-based setlist slot */ slot: number; /** "1A Clean" */ label: string }[] };
}

export type SystemGroup = "Preset" | "Scene" | "Tuner" | "Tempo" | "Mode" | "Gig View" | "Looper" | "Page";

/** dev/remote_footswitch.rs:243-256 */
export interface FootswitchBlock {
  /** Grid row 0..=3 */
  row: number;
  /** Grid column 0..=7 */
  column: number;
  name: string;
  /** "Row 1 slot 2" */
  location: string;
  parameters: { index: number; name: string }[];
}

/** dev/remote_footswitch.rs:463-487, ui/custom_mode.rs:65-93 */
export interface SlotView {
  /** null = empty slot. */
  action: SlotActionView | null;
  /** Typed label, else the action's name; "" when empty. */
  label: string;
  /** Typed label only; null = uses the action's name. */
  customLabel: string | null;
  /** "Bypass · momentary", "Load 3B", "PC 12 · Ch 1". */
  detail: string;
  /** The LED/tile colour in use, "#rrggbb". */
  colour: string;
  /** Chosen colour index 0..=7; null = auto. */
  swatch: number | null;
  /** Its scene is active / block is on / screen shows. */
  engaged: boolean;
  /** false: missing block or preset, or the command's extension is not running. */
  available: boolean;
  /** Parameter actions: percent. Else null. */
  value: number | null;
  /** Bypass actions: boolean. Else null. */
  momentary: boolean | null;
  /** Send MIDI actions. */
  messages: MidiMessage[];
}

/** Action as the view shows it. dev/remote_footswitch.rs:490-508 */
export type SlotActionView =
  | { kind: "system"; action: SystemActionJson }
  /** parameter null = bypass. */
  | { kind: "block"; row: number; column: number; parameter: number | null }
  /** Any preset (the loaded one or a setlist slot); the slot's label names it. */
  | { kind: "loadPreset" }
  | { kind: "sendMidi" }
  | { kind: "command"; key: string; command: string };

/** serde remote SystemDef, externally tagged, camelCase. dev/mappings.rs:125-171.
 *  Examples from tests: "tuner", { scene: 2 }, { customPage: "next" }, { customPage: { go: 1 } }. */
export type SystemActionJson =
  | "nextPreset" | "previousPreset" | "nextScene" | "previousScene"
  | "tuner" | "tapTempo" | "nextMode" | "gigView"
  /** 0..=7 = A..=H */
  | { scene: number }
  | { mode: "preset" | "scene" | "stomp" | "custom" }
  | { looper: LooperButton }
  | { customPage: "next" | "previous" | { go: number } }
  /** Accepted by the parser, but a slot cannot press another slot (422). Mappings only. */
  | { customSlot: number };

export type LooperButton =
  | "recordOverdub" | "playStop" | "undo" | "reverse" | "halfSpeed" | "oneShot" | "punchIn" | "duplicate";

/** dev/remote_footswitch.rs:122-138 (deny_unknown_fields), :417-448, :463-472 */
export type MidiMessage =
  /** values = [cc#, value, null] */
  | { kind: "cc"; channel: number; values: [number, number, number | null] }
  /** values = [cc#, min, max] */
  | { kind: "ccToggle"; channel: number; values: [number, number, number] }
  /** values = [bank CC#0 | null, bank CC#32 | null, program] */
  | { kind: "pc"; channel: number; values: [number | null, number | null, number] };
// channel 1..=16; every value 0..=127. In a command, the third value of "cc" is ignored
// (any value or null); the view returns it as null.
```

### Commands

```ts
/** POST /api/footswitch/command body. serde tag "op", camelCase. dev/remote_footswitch.rs:25-47 */
export type FootswitchCommand =
  /** page 0..pages-1, slot 0..=9. */
  | { op: "slot"; scope: LayoutScope; page: number; slot: number; edit: SlotEdit }
  /** Page `to` becomes a copy of page `from`. */
  | { op: "copyPage"; scope: LayoutScope; from: number; to: number }
  /** true: THIS PRESET gets a copy of ALL PRESETS; false: it loses its own layout. */
  | { op: "presetLayout"; own: boolean }
  /** Read only: the reply is the view plus `presets`. */
  | { op: "presets"; setlist: string };

export type LayoutScope = "global" | "preset";

/** serde tag "kind", camelCase. dev/remote_footswitch.rs:56-91, checks :315-367 */
export type SlotEdit =
  | { kind: "action"; action: SlotActionInput }
  | { kind: "clear" }
  /** ≤ 16 chars; "" = the action's name. */
  | { kind: "label"; label: string }
  /** 0..=7, or null = auto from the action. */
  | { kind: "colour"; colour: number | null }
  /** 0..=100, parameter action only. */
  | { kind: "value"; value: number }
  /** Bypass action only. */
  | { kind: "momentary"; momentary: boolean }
  /** The whole list (≤ 12). Send MIDI action only ("Choose Send MIDI first"). */
  | { kind: "messages"; messages: MidiMessage[] }
  /** The target slot of the same layout becomes a copy of this one. */
  | { kind: "copyTo"; page: number; slot: number }
  /** The two slots swap. */
  | { kind: "moveTo"; page: number; slot: number };

/** serde tag "kind", camelCase. dev/remote_footswitch.rs:94-120, resolve :373-414 */
export type SlotActionInput =
  | { kind: "system"; action: SystemActionJson }
  /** `parameter` omitted or null = bypass; else a parameter index of that block. */
  | { kind: "block"; row: number; column: number; parameter?: number | null }
  /** The loaded preset (refused when no preset is loaded or it has no file). */
  | { kind: "loadPreset" }
  /** Command only: the preset in `slot` (0-based) of `setlist`. Shown back as loadPreset. */
  | { kind: "preset"; setlist: string; slot: number }
  | { kind: "sendMidi" }
  /** An extension command from `commands`. */
  | { kind: "command"; key: string; command: string };
```

Rules:

- Each edit starts from the state of the last reply, and a message list is sent whole. The page
  sends one command at a time and waits for its reply (`parts/footswitch.html:133-157`).
- Typical refusals (422): `There is no page N`, `There is no switch N`, `There is no scene N`,
  `There is no block there`, `<block> has no parameter N`, `A slot cannot press another slot`,
  `A label has at most 16 characters`, `There are eight colours`, `A channel is 1 to 16`,
  `A message needs each of its values`, `A MIDI value is 0 to 127`, `That slot is empty`,
  `This preset uses the layout of all presets` (edit of THIS PRESET with no own layout),
  `No extension adds a Custom footswitch mode`, `Not a command: <serde error>`.
  (`dev/remote_footswitch.rs:299-447`, `dev/custom.rs:834-1014`.)
- Playing the switches (Gig View taps, page turns) is not in the API (`dev/remote_footswitch.rs:5`).

## 8. Extension files and tabs: client rules

From `page.html:265-393` and `dev/remote_files.rs`:

- File name: an extension from the list (case-insensitive), stem of `[A-Za-z0-9 ._-]`, 1..=48
  bytes, not starting with `.` (`dev/remote_files.rs:38-52`,
  `crates/tetrad-extensions/src/store.rs:865-871`). The page cleans a name like this:

  ```js
  const stem = name.slice(0, name.length - extension.length)
    .replace(/[^A-Za-z0-9 ._-]/g, "").replace(/\s+/g, " ").trim().replace(/^\.+/, "")
    .slice(0, MAX_FILE_NAME).trim();   // MAX_FILE_NAME = 48
  ```

- Upload: pieces of 48 KiB (`FILE_CHUNK = 48 * 1024`), `offset=0` first (starts a `.part`
  file), then exactly where the part ends, `last=1` on the last piece. An empty file still sends
  one piece at offset 0 (refused: `<name> is empty`). Max 3 MiB; max 64 files per extension
  (`dev/extension_jobs.rs:27`, `dev/remote_files.rs:119-131`). The old file stays until the last
  piece.
- After an upload, poll `GET /api/files` every 2 s while any file is `"waiting"` (page: up to 15
  times) (`page.html:303-308`).
- Tabs: `GET /api/tabs/page?id=` gives HTML that expects a `tetrad.send(message)` bridge
  (`page.html:431-452`). Each `tetrad.send` becomes `POST /api/tabs/call?id=` with
  `JSON.stringify(message ?? null)`. A non-JSON body is 422 `The message is not JSON`. In a
  native app, host it in a WebView and forward messages the same way.
- The page re-reads files, tabs and every part every 8 s while visible and not uploading
  (`page.html:504-509`).

## 9. Differences from docs/remote.md and other surprises

- `docs/remote.md:310-324` ("Calls") lists no `GET /api/status`, `/api/files*` or `/api/tabs*`.
  Files and tabs are in other sections (`:202-204`, `:234-235`); `/api/status` is in no doc.
- `/api/library/*` and `/api/backup/*` check the query before the PIN (400 can come before 401).
  The other routes check the PIN first.
- `POST /api/tabs/call` can return **200 with `{"error": ...}`** when the extension answers bad
  JSON (`api.rs:421-428`). Check for `result` in the body, not only the status.
- `GET /api/status` returns **422** when the library is closed, but the PIN is right; the page
  treats it as logged in (`page.html:173-178`).
- `GET /api/footswitch` with no Custom mode is **200 `{"mode": null}`**, but every command is 422.
- Upload names need 1–60 UTF-16 units; rename and new-folder names need 4–60.
- Mapping row `id` and action `key` are Rust `Debug` strings: opaque and not stable across
  firmware builds.
- The `Host` must be a numeric IPv4 address: mDNS names (`*.local`) get 421.
- No keep-alive: one TCP connection per call. The body limit (64 KiB) is why every upload goes in
  48 KiB pieces.
- Ambiguous: no API reports the Remote on/off state or the PIN. The PIN is only on the unit or in
  its QR (`#pin=NNNN`). A 503 `Remote is off` means the user must turn Remote on at the unit.
- Ambiguous: `/api/backup/file` with `offset == size` is allowed and returns whatever
  `read_at` gives (likely 0 bytes). The page stops at `offset < size` and treats an empty piece
  before the end as "The backup ended early" (`parts/backup.html:104-112`).
