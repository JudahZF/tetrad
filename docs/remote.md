# Remote

Remote is a web page on your Quad Cortex. Use it to add IRs from a computer or a phone on the
same Wi-Fi network, with no app to install.

The Quad Cortex cannot show up as a USB drive: its USB port belongs to the stock DSP firmware,
not to Linux, where Tetrad runs. Remote uses Wi-Fi instead.

## Use Remote

1. Connect the unit to Wi-Fi (**Settings → System → Connection**).
2. Open **Settings → System → Remote** and turn on **Remote**. The page shows the address, for
   example `http://192.168.1.20`, and a 4-digit PIN.
3. On a computer or phone on the same network, open the address in a browser.
4. Enter the PIN. The browser keeps it until the PIN changes.
5. Drop IR files on the page, or tap the box to choose them. The page uploads them one at a
   time and shows the result of each file.

Each IR goes into the IR saving destination: the folder you chose with **SET AS SAVING
DESTINATION** in the IR browser, or the IR root folder. The unit shows a short message for each
IR it adds. Its name is the file name without the extension, at most 60 characters.

While Remote runs, the first row of the ⋮ menu (top right of the Grid) shows the PIN, as
`Remote #1234`. A tap on it opens this page of Settings.

Remote stays on after a restart if you leave it on. It gets a new PIN at each start. If the
port does not open, the switch stays on and the page shows why; turn it off and on to try
again.

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

`/media/p4/tetrad/remote.json` holds the on/off setting only, for example
`{"enabled":true}`. The PIN is never saved.
