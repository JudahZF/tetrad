# Boot launcher and boot selector

The launcher decides at each boot which UI starts: stock ZenUI or Tetrad (`tetrad`). It is a
BusyBox `ash` script on the unit. It is made to fail safe: when it is not sure, it starts stock.

## Files

All Tetrad files are on the user data partition (p4), in `/media/p4/tetrad/`:

| File | What it is |
| --- | --- |
| `bin/tetrad` | The Tetrad device binary |
| `launcher/tetrad-launch.sh` | The launcher. The boot hook runs it in place of `./ZenUI` |
| `launcher/tetrad-launcher.sh` | Operator commands (status, arm, disarm, chooser, hook) |
| `launcher/tetrad-update.sh` | The CorOS update wrapper (see [updates.md](updates.md)) |
| `launcher/select` | The md5 of the armed `bin/tetrad`. No file: Tetrad is disarmed |
| `launcher/disabled` | The reason why the launcher stopped starting Tetrad |
| `launcher/pending` | The count of Tetrad starts that did not confirm start-up |
| `launcher/boot-choice` | The saved boot selector choice: `tetrad` or `coros` |
| `launcher/no-chooser` | When present, the boot selector does not show |
| `launcher/launch.log` | The launcher log (the newest 32 KiB, then `launch.log.1`) |

The launcher also writes each log line to the system log.

## The boot hook

The hook is the only change to the root file system. Stock `/etc/init.d/S80drvinit` starts
`./ZenUI` on one line. `tetrad-launcher.sh install-hook` replaces that line with two lines:

1. Set the UI command to `./ZenUI`. When `tetrad-launch.sh` exists and `sh -n` can parse it, set
   the UI command to `sh tetrad-launch.sh`.
2. Start the UI command with the same environment and the same log pipe as stock.

So a missing or broken launcher always gives stock ZenUI.

`install-hook` and `remove-hook` change the file only when its md5 is a known value: the CorOS
4.1.0 file (also used by 4.1.1) or the hooked file. They write the new file next to the old one,
check its md5, and then rename it into place. On an unknown file they refuse and change nothing.

## What one boot does

`tetrad-launch.sh` runs these checks in this order. The first check that fails starts stock ZenUI.

1. Name the shell `ZenUI`, so that ZenWatchdog sees a UI process (see [ZenWatchdog](#zenwatchdog-and-the-process-name-zenui)).
2. `select` must exist (Tetrad is armed).
3. `disabled` must not exist.
4. `bin/tetrad` must exist and be executable.
5. The md5 of `bin/tetrad` must be the md5 in `select`.
6. `pending` must be less than 3. At 3 or more, the launcher writes `disabled` and starts stock.
7. The factory files must exist. If not, the [first-boot rule](#first-boot-after-a-coros-update)
   applies.
8. The [boot selector](#boot-selector) runs. "CorOS" starts stock. No start is counted.
9. The launcher adds 1 to `pending` and runs `tetrad run --started launcher/pending`.

Tetrad deletes `pending` when its start-up ends with the DSP running. So three starts in a row
that crash, hang, lose power or fail to start the DSP make the fourth boot start stock.

Stock always starts as `exec ./ZenUI`: it is the same process that init starts without the hook.

### Exit codes of `tetrad run`

| Code | Meaning | Launcher action |
| --- | --- | --- |
| 0 | A power action (restart or power off). The stock teardown runs | none |
| 4 | The stock firmware is not a supported CorOS build. Nothing was opened | writes `disabled`, starts stock in the same boot |
| 126, 127 | The binary could not start | starts stock in the same boot |
| other | A failure | none: ZenWatchdog restarts the unit, and `pending` counts the start |

`tetrad run --check` opens nothing. It exits 0 for a supported build and 4 for an unsupported
build.

## Boot selector

When Tetrad is armed and could start, the launcher runs `tetrad select --file launcher/boot-choice`
before either UI. The screen shows:

- two large buttons, **CorOS** and **Tetrad**,
- a mark on the default button,
- a 5 second countdown,
- the Tetrad version.

A touch on a button starts that UI at once. After 5 seconds the default starts. The default is
the saved choice in `boot-choice`. When the file is missing or holds another word, the default is
Tetrad. The selector writes the file only when a touch picks the UI that is not the default.

| `tetrad select` exit | Meaning |
| --- | --- |
| 10 | CorOS: the launcher starts stock ZenUI |
| 11 | Tetrad: the launcher continues with step 9 |
| 12 | Tetrad dev (a dev build only): the launcher continues with step 9 and starts `tetrad run --dev` |
| any other | A failure: the launcher starts the saved choice, with no screen |

The codes 10 and 11 are not codes that a crash, a kill or a command-line error gives. So an old
binary without `select` also gives the saved choice.

The launcher kills a selector that runs for more than 8 seconds. The boot never waits on the
selector. The selector saves the boot splash rows of the framebuffer and writes them back before
it exits.

A dev build ([development.md](development.md#dev-build)) shows a third button, **Tetrad dev**. It
starts Tetrad with `--dev`, which writes the `perf` statistics to the log. It saves as `tetrad-dev`
in `boot-choice` when it is not the default. A release build never gives exit 12 and reads
`tetrad-dev` as an unknown word, so it starts Tetrad. When a dev selector fails, the launcher starts
the saved choice without `--dev`.

To turn the selector off: `tetrad-launcher.sh chooser off`, or the **Show at boot** switch in
Settings → Tetrad → Boot Selector. Then an armed boot starts Tetrad with no screen.
`tetrad-launcher.sh chooser on`, or the switch, turns it on again. Both write and remove the same
`no-chooser` file. Tetrad reads the file at its first preset load.

Checked on the unit: armed boots with each saved choice, the screen, the countdown and a touch in
a manual run. Not checked on the unit: a touch during a real boot, the 8 second kill, and
`chooser off`.

## First boot after a CorOS update

A new root partition does not have three factory files that stock ZenUI makes at its first start:
`factory.db`, `factory_ir_entities.pb` and `searchIndexFactory` in `/opt/neuraldsp`. Tetrad reads
these files but never writes them. When one is missing or empty, the launcher:

1. Runs `tetrad run --check`.
2. Exit 0: starts a watcher process, then starts stock ZenUI. No selector shows. No start is
   counted.
3. Exit 4: writes `disabled` and starts stock. No watcher starts.
4. Other exit: starts stock. No watcher starts.

The watcher (`tetrad-launch.sh first-boot-watch`) checks every 10 seconds. When the three files and
the stock marker `captureConversionFinished` exist, it waits 90 seconds more. It then waits while
a CorOS install runs (`/etc/updater_status` holds `UPDATER_START` or `UPDATER_STEP_*`). Then it
restarts the unit, and the next boot starts Tetrad. After 20 minutes of waiting it stops and does
not restart the unit.

## ZenWatchdog and the process name `ZenUI`

Stock ZenWatchdog runs `pidof ZenUI` every 500 ms. After 3 seconds with no `ZenUI` process it
counts down 5 seconds and restarts the unit. It never restarts the UI by itself.

So the UI process must have the stock name `ZenUI`, also when it is Tetrad. The binary file is
`tetrad`, but its process name is `ZenUI`. Do not change this name: with another name,
ZenWatchdog restarts the unit 8 seconds after each boot.

The launcher names its shell `ZenUI` at once. The md5 check of the binary takes about 2
seconds. Before the launcher starts Tetrad, it names the shell `tetrad-launch`, and Tetrad names
itself `ZenUI` first thing. Only one process has the name `ZenUI` at a time, so a dead Tetrad
still makes ZenWatchdog restart the unit. That restart is the retry, and `pending` stops a loop.

## Commands

Run these on the unit as root (`sh /media/p4/tetrad/launcher/tetrad-launcher.sh COMMAND`):

| Command | What it does |
| --- | --- |
| `status` | Shows the hook state, the armed md5, the selector state, `disabled`, `pending` and the last log lines |
| `arm` | Arms the binary that is now in `bin/`: writes its md5 to `select`, deletes `disabled` and `pending` |
| `disarm` | Deletes `select`. Stock starts from the next boot. The hook stays |
| `chooser on` / `chooser off` | Turns the boot selector on or off |
| `install-hook` | Adds the hook to `S80drvinit` |
| `remove-hook` | Puts back the original `S80drvinit`, byte for byte |

Exit codes: 0 for success, 1 for a failure (nothing changed), 2 for a usage error.

After each upload of a new `bin/tetrad`, run `arm` again. Until then the md5 check fails and
stock starts.

Stock ZenUI stops processes that it does not know after a few seconds. Keep commands short while
stock runs.

## Recovery

| Situation | Way back to stock |
| --- | --- |
| The boot selector shows | Touch **CorOS**. The choice is saved |
| Tetrad runs and SSH works | `tetrad-launcher.sh disarm`, then restart the unit |
| Tetrad crashes or hangs before its start-up ends | Nothing to do: after 3 starts, the 4th boot starts stock |
| Tetrad hangs after its start-up, no SSH | Power off during the start-up, before the UI shows, 3 times in a row |
| No SSH | Use the serial console, or put the SD card in a computer: on partition 4 (ext4) delete `tetrad/launcher/select` |
| The hook must go, no network | SD card in a computer: mount the active root partition and run `TD_ROOT=/path/to/root sh tetrad-launcher.sh remove-hook` |
| A CorOS update went wrong | See [updates.md](updates.md#roll-back) |

To remove Tetrad fully: `disarm`, then `remove-hook`, then restart. You can then delete
`/media/p4/tetrad`.

## Host tests

`crates/tetrad-app/tests/device_launcher.rs` runs the launcher scripts in a temporary fake root.
The tests cover each check above, the selector exit codes, the first-boot rule and the hook edit.
They also check that the constants in the scripts equal the Rust constants. Set
`TD_LAUNCHER_TEST_SH` to run them with another shell, for example the unit's BusyBox under
QEMU.

`crates/tetrad-app/tests/install_script.rs` runs `scripts/install.sh` against a fake unit:
install, update, uninstall and the move from zen-rs. Set `TD_REMOTE_PATH` to a folder of BusyBox
applet wrappers to run the remote commands with the unit's tools.
