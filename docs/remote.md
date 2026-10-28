# Remote

Remote is a web page on your Quad Cortex. Use it from a computer or a phone on the same Wi-Fi
network, with no app to install, to:

- add IRs and manage the IR library,
- edit mappings and the Custom footswitch layouts,
- download a backup of your data, or restore one,
- make and choose themes,
- manage extensions,
- read the notices of the unit, and download the support file for a bug report
  ([support.md](support.md)).

The Quad Cortex cannot show up as a USB drive: its USB port belongs to the stock DSP firmware,
not to Linux, where Tetrad runs. Remote uses Wi-Fi instead.

## Use Remote

1. Connect the unit to Wi-Fi (**Settings → System → Connection**).
2. Open **Settings → Tetrad → Remote** and turn on **Remote**. The page shows the address, for
   example `http://192.168.1.20`, a 4-digit PIN and a QR code.
3. On a phone, scan the QR code: it opens the page with the PIN, so you do not type it. Or open
   the address in a browser on a computer or phone on the same network, and enter the PIN.
4. The browser keeps the PIN until the PIN changes.

The QR code holds the address and the PIN after a `#`, for example
`http://192.168.1.20/#pin=4821`. A browser does not send the part after the `#` to the unit, and
the page removes it from the address bar after it reads the PIN.

To see the PIN without this page, turn on **Show PIN in menu**. While Remote runs, the first
row of the ⋮ menu (top right of the Grid) then shows the PIN, as `Remote #1234`. A tap on it
opens this page of Settings.

Both switches stay as you set them after a restart. Remote gets a new PIN at each start. If
the port does not open, the Remote switch stays on and the page shows why; turn it off and on
to try again.

## The pages

The pages are in a sidebar. On a phone the sidebar becomes a row of buttons above the page.

| Group | Pages |
| --- | --- |
| Library | **IRs** |
| Controls | **Mappings**, **Footswitch** |
| Unit | **Backup**, **Themes**, **Notices** |
| Extensions | **Manage**, then one page for each extension that takes files and one for each extension tab |
| Developer | **Update** (dev builds only) |

The top of every page has **Support file** (it downloads the support file) and **Change PIN**.
The line under the title names the folder that new IRs go to.

## IRs

Drop IR files on the box, or tap it to choose them. The page uploads them one at a time and
shows the result of each file.

Each IR goes into the IR saving destination: the folder you chose with **SET AS SAVING
DESTINATION** in the IR browser or on this page, or the IR root folder. The unit shows a short
message for each IR it adds. Its name is the file name without the extension, at most 60
characters. An IR with the same samples as an IR in your library replaces that IR (the IR's id is
made from its samples), as a Cortex Control import does: an upload of the same audio under a new
name looks like a rename.

**Library**, under the box, lists your IRs (not the factory IRs):

- **Search** filters by name. The folder list shows the folders as a tree. The root (**My IRs**)
  shows every IR; the folder that new IRs go to is marked **(new IRs)**.
- **Save new IRs here** makes the folder in the list the saving destination.
- **New folder** makes a folder in the folder in the list. There can be at most 30 IR folders
  (the root counts) and 4 levels.
- Each IR shows its date and its folders. **Folders…** puts the IR in a folder; a folder with a ✓
  holds the IR, and choosing it takes the IR out. An IR stays in the root.
- **Rename** and **Delete** do what they do in the IR browser. Names of IRs and folders have 4 to
  60 characters. A preset that uses a deleted IR keeps its block, which then shows the old name
  or `DELETED IR`, as in stock.

The list follows the changes made on the unit or in Cortex Control (the page reads it again
every 8 seconds).

<!--controls-->

## Backup

**Backup** (under **Unit**) saves your data on a computer, and puts it back.

- **Download backup** packs a backup on the unit, then the browser saves
  `QC-Backup-YYYYMMDD-HHMM.tar.gz` (UTC). The page removes the unit's copy after the download.
- A backup holds the stock backup set, the same as a cloud backup: presets and setlists, IRs and
  captures (in the library database), plugin presets, `ZenjackConfig.ini` and the favourites.
  Tetrad's own settings (themes, mappings, footswitch layouts, extensions) are not in it.
- **Restore**: drop a backup (`.tar.gz`, at most 512 MiB) on the box. The page sends it in
  pieces and then says to touch **RESTORE** on the unit. Nothing is restored before that touch:
  **Settings → Tetrad → Remote** shows the file with **DISCARD** and **RESTORE**, and RESTORE asks
  again. The restore then replaces the presets, setlists, IRs, captures, plugin presets and
  favourites with the backup, keeps the Wi-Fi settings, and reloads, as a cloud restore does.
- The unit checks every file in the backup before it removes anything. A backup with other
  files than the backup set is refused. Input and return levels are not converted: the unit
  takes the backup as its own.
- One backup at a time: a new upload replaces the one that waits, and a download is refused
  while a received backup waits (discard it first) or while another backup runs (cloud, Cortex
  Control or Remote). Turning Remote off or on removes the backup files of this page.


## Themes

**Themes** lists every theme the unit can show (built in, theme files and themes of extensions,
[themes.md](themes.md)), with a strip of its colours. **In use** marks the theme on the screen,
and a theme that fails its checks shows why. **Use** shows a theme on the unit at once.

The editor makes theme files:

- **New theme** starts from Stock, **Edit as copy** from a built-in or extension theme, and
  **Edit** opens a theme file. The ID follows the name until you type an ID.
- Each colour has a colour picker. **From base** means the colour comes from the base theme;
  **Set** means the theme sets it, and **Reset** takes it out again. Divider, Accent fill and On
  accent show **Auto** until you set them.
- The preview is a small copy of the unit screen in the editor's colours. Block colours do not
  change with a theme.
- **Save** writes `/media/p4/tetrad/themes/<id>.json`; **Save and use** also shows the theme on
  the unit. The unit checks the theme with the same rules as a file on the card, and the reason
  shows under the editor when it refuses one (for example `muted on background has a contrast
  of 2.1:1; it needs 4.5:1`). A save with the ID of a theme file replaces that file, after a
  question. The IDs of the built-in themes are refused, and so is a 33rd theme file.
- **Delete** removes a theme file. When it was in use, the unit shows Stock.
- **Download JSON** downloads a theme file; **Import JSON** loads one into the editor. Colours
  that the editor does not know are left out, and the page lists them.

## Notices

**Notices** shows what the unit reported recently (the same notices as in the support file),
newest first, with the time of day from your browser's clock. Choose a part of the unit or type
words to filter them. The serial, the sign-in tokens and the account names are taken out. The
unit keeps the last 256 notices.

## Manage extensions

**Manage**, at the top of the **Extensions** group, does what **Settings → Tetrad → Extensions**
does on the unit:

- Each installed extension shows its version, repository, description and state (**Running**,
  **Starting…**, **Off**, or **Faulted** with the reason), its settings, its command buttons,
  status lines, DSP blocks, themes, footswitch mode and its log.
- **Turn on**/**Turn off**, **Update to …**, **Retry** (when faulted) and **Remove** do what they
  do on the unit. Remove asks first; the extension's settings and stored data stay, so a new
  install keeps them. A setting change is saved and restarts the extension, as on the unit.
- **Available** lists what the enabled repositories offer, with **Install**. **Refresh** reads
  the catalogs again.

The page installs only from repositories that the unit already trusts, and it cannot take a
package file. Add, trust, turn off or remove a repository on the unit. When an extension is
turned on or off, installed or removed, its file and tab pages in the sidebar follow.

## Extension tabs

An extension can add its own page to Remote (for example a list of its models with information
that Tetrad does not know), under **Extensions** in the sidebar. The page of an extension loads
when you open it, in a frame that cannot reach the PIN, the other pages or the network; its only
way to the unit is through the extension that supplied it. Authors: [extensions.md](extensions.md#add-a-tab-to-the-remote-page).

The calls are `GET /api/tabs` (the extension tabs), `GET /api/tabs/page?id=<extension id>` (the page, as
JSON with `html`) and `POST /api/tabs/call?id=<extension id>` (a JSON message in the body; the
answer is `{"result": ...}` or `{"error": ...}`), all with the PIN. An extension that is off, or
that has no handler, answers with an error.

## Extension files

Some extensions build their DSP blocks from files that you copy to the unit, such as the models
of Neural Amp Modeler A1 and A2 ([extensions.md](extensions.md#neural-amp-modeler)). When one of these
extensions is installed, the Remote page has a page for it in the sidebar, under **Extensions**, named like the extension (for example **Neural Amp Modeler A1**):

- **Drop files** on the box, or tap it to choose them. The page sends each file in pieces of 48 KB
  and shows the progress. A file goes to the folder that the extension reads
  (`/media/p4/tetrad/assets/<extension id>/`). The unit keeps the old file with the same name
  until the new one is complete.
- The page changes a file name to what the extension reads: letters, digits, spaces, `.`, `_`
  and `-` (48 characters at most, without the extension). A `+` or `(` goes away, and the line
  of the upload says **Stored as** with the new name. A file must end in one of the extensions
  that the extension lists (`.nam` for NAM), and it is at most 3 MiB. An extension reads 64
  files at most.
- **The list** shows each file with its size and what the extension made of it: **Ready** (its
  block exists), **Reading…** (the extension has not built it yet; the page asks again every 2
  seconds) or **Failed**, with the reason (for example that it is an A2 model and the A1
  extension does not play it).
- **Delete** removes a file from the unit, after a confirmation. Its block goes from the lists.

The extension reads its folder again after each upload or deletion, so a new block shows up
without turning the extension off and on. An extension that is switched off reads the folder when
it starts. A piece that did not finish stays as `<name>.part` in the folder; the extension does
not read it, and the next upload of that file starts it again.

The same PIN protects these calls, and Remote has to be on, as for IRs. The calls are
`GET /api/files`, `POST /api/files/chunk?id=<extension id>&name=<file>&offset=<bytes>[&last=1]`
(the piece is the body) and `POST /api/files/delete?id=<extension id>&name=<file>`.

## Update Tetrad (dev build)

A dev build ([development.md](development.md#dev-build)) also takes a Tetrad update on the
Remote page: the `tetrad` binary and, when they change, the launcher scripts. This is for a unit
that you cannot reach over SSH. A release build has none of this: no update part in the page,
no update routes (they answer 404), and no way to send it a program.

1. `scripts/build-dev.sh` builds the dev build and leaves `tetrad-update.tar.gz` beside the
   binary. With `--scripts` the archive also has the three launcher scripts of your checkout.
   `scripts/dev-update.sh archive BINARY [--scripts]` makes the same file from a binary you
   have.
2. Open the Remote page and enter the PIN. A part **Update Tetrad** shows the running version.
3. Choose or drop `tetrad-update.tar.gz`. The browser opens it and sends the files in it, or you
   can choose the files themselves. The unit takes only `tetrad`, `tetrad-launch.sh`,
   `tetrad-launcher.sh` and `tetrad-update.sh`, each within its size limit (64 MiB for the
   binary, 256 KiB for a script), and refuses a `tetrad` that is not a 32-bit ARM executable.
4. When the page says it is ready, touch UPDATE TETRAD in Settings → Tetrad → Tetrad Updates.
   Nothing installs before that. The unit then checks each script with `sh -n` and the binary
   with `tetrad run --check` (the installed binary, when the update has only scripts), moves
   the files in place and arms the binary, as for an update over USB
   ([usb.md](usb.md#install-a-release-over-usb)).

From a terminal: `scripts/dev-update.sh push tetrad-update.tar.gz <unit IP> <PIN>` does the same
steps, and `scripts/dev-update.sh log <unit IP> <PIN>` prints the recent notices of the unit (the
log), which a dev build also gives at `GET /api/log`.

A push survives a flaky network: when a request is dropped (the connection ends with no answer),
`push` starts over from `begin` with the files cut in pieces of another size (49152, 40000, 32768,
then 24576 bytes), up to four attempts. A drop can depend on where a piece starts: on the first
unit a build always failed at the same piece (`Scene Bypass Behavior` in a UI string, 1.5 MB in) and went
through with pieces of 40000 bytes. `TETRAD_CHUNK=<bytes>` sets the size. A refusal by the unit
is not retried.

A dev build can also start the install without the touch: `POST /api/update/install` (with the PIN)
does what UPDATE TETRAD does, with the same refusals (no staged files, a CorOS update running),
and the unit restarts when it is done. `scripts/dev-update.sh install <unit IP> <PIN>` calls it,
and `push ... --install` sends the files and starts the install. So a build can be sent, installed
and checked in the log with no one at the unit. A build that does not start leaves the unit to
the launcher: three starts in a row that never confirm start-up bring back stock CorOS
([launcher.md](launcher.md#recovery)).

### Without reading the PIN: a key

The PIN is new at every start of Remote, so a script would need it from you each time. A dev
build can give it to a key that you add once:

1. `scripts/dev-update.sh key ~/.config/tetrad-dev/remote.pem` makes a private key (it stays on
   your computer; the `build with:` line it prints is for signed updates over USB and does not
   matter here) and prints the public key.
2. `scripts/dev-update.sh authorize <unit IP> <PIN> ~/.config/tetrad-dev/remote.pem` sends the
   public key to the unit (`POST /api/keys`, with the PIN that the unit shows now). The unit
   keeps up to 8 public keys, one per line, in `/media/p4/tetrad/remote-keys`.
3. After that, `key:~/.config/tetrad-dev/remote.pem` takes the place of the PIN in `push`,
   `install`, `log` and `status`. The script asks the unit for a one-time challenge
   (`GET /api/auth/nonce`), signs `tetrad-remote-pin:` and the challenge with the private key
   (Ed25519), and sends the signature (`POST /api/auth/pin`). The unit answers with its PIN when
   the key is known and the signature matches.

The secret (the private key) never goes over the network. A challenge works once and for 60
seconds, at most four wait at once, and the unit's log names the key (its first 8 characters) and
whether it got the PIN, never the signature. The page and the PIN header still use plain HTTP, so
someone who watches your network when the script then uses the PIN can read that PIN, as with a
PIN that you type in the page; it is valid until Remote starts again. To take a key away, delete
its line in `remote-keys` on the card.

Nobody signs these files. What protects the unit is the PIN and that this exists only in dev
builds; the touch on the unit is the third protection, unless the install call is used. The page uses plain HTTP: anyone on your network who knows the PIN
can stage a program, and install it with the install call. A launcher script
runs as root at boot in place of the stock start, and `sh -n` finds only syntax errors: change
the scripts rarely, and keep a way back (the SD card in a computer,
[launcher.md](launcher.md#recovery)). A new `begin` or a refused step ends the staging: send the
files again.

## IR files that work

The page converts each file in the browser before the upload, so most IR files work:

- WAV or AIFF, at any sample rate and bit depth. The browser must be able to decode the file.
- Mono or stereo. A stereo IR is mixed to mono (half left plus half right).

The unit keeps the first 1,024 samples at 48 kHz (21 ms), as stock CorOS does with an IR from
Cortex Control or the cloud. A longer IR is cut at that point. The page sends the stock format:
mono, 48 kHz, 32-bit float WAV.

An IR with the same samples as an IR in your library replaces that IR, as a Cortex Control
import does.

## The PIN and your network

- The page asks for the PIN before it shows anything from the unit or takes an upload.
- Each wrong PIN gets its answer after 1 second. After 10 wrong PINs the unit makes a new PIN,
  and the browsers that knew the old PIN ask again.
- The page uses plain HTTP on port 80, because the unit has no certificate. Someone who can read
  the traffic on your network can read the PIN. Turn Remote off on networks you do not trust,
  for example at a venue.
- The page talks only to the unit. Other websites cannot upload through your browser: the
  requests need the PIN header, the unit answers no cross-site requests, and it answers only
  requests addressed to its IP address. A name that points to the unit (DNS rebinding) gets
  no page.

## Problems

| The page or the unit says | What to do |
| --- | --- |
| Connect to Wi-Fi first | The unit has no Wi-Fi address. Connect it to Wi-Fi. |
| The unit is not reachable | Check that the computer is on the same network, and that Remote is on. |
| Port 80 is in use | Another program on the unit uses port 80. Restart the unit. |
| Wrong PIN | Enter the PIN that the Remote page shows now. |
| Open the page by the unit's IP address | Type the address as the unit shows it (numbers), not a name. |
| Not an audio file | The browser cannot decode the file. Export it again as WAV. |

## Settings file

`/media/p4/tetrad/remote.json` holds the two switches only, for example
`{"enabled":true,"pin_in_menu":false}`. A file without `pin_in_menu` means off. The PIN is
never saved.
