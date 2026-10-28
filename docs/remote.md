# Remote

Remote is a web page on your Quad Cortex. Use it to add IRs from a computer or a phone on the
same Wi-Fi network, with no app to install, and to download the support file for a bug report
([support.md](support.md)).

The Quad Cortex cannot show up as a USB drive: its USB port belongs to the stock DSP firmware,
not to Linux, where Tetrad runs. Remote uses Wi-Fi instead.

## Use Remote

1. Connect the unit to Wi-Fi (**Settings → System → Connection**).
2. Open **Settings → Tetrad → Remote** and turn on **Remote**. The page shows the address, for
   example `http://192.168.1.20`, and a 4-digit PIN.
3. On a computer or phone on the same network, open the address in a browser.
4. Enter the PIN. The browser keeps it until the PIN changes.
5. Drop IR files on the page, or tap the box to choose them. The page uploads them one at a
   time and shows the result of each file.

Each IR goes into the IR saving destination: the folder you chose with **SET AS SAVING
DESTINATION** in the IR browser, or the IR root folder. The unit shows a short message for each
IR it adds. Its name is the file name without the extension, at most 60 characters.

To see the PIN without this page, turn on **Show PIN in menu**. While Remote runs, the first
row of the ⋮ menu (top right of the Grid) then shows the PIN, as `Remote #1234`. A tap on it
opens this page of Settings.

Both switches stay as you set them after a restart. Remote gets a new PIN at each start. If
the port does not open, the Remote switch stays on and the page shows why; turn it off and on
to try again.

## Extension files

Some extensions build their DSP blocks from files that you copy to the unit, such as the models
of Neural Amp Modeler A1 and A2 ([extensions.md](extensions.md#neural-amp-modeler)). When one of these
extensions is installed, the Remote page shows a part for it, below the IR box:

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

## Files that work

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
