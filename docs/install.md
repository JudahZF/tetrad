# Install Tetrad on your Quad Cortex

This guide tells you how to install, update, check, turn off and remove Tetrad on your own
Neural DSP Quad Cortex. One script on your computer does all steps: `scripts/install.sh`.

Tetrad is not made by Neural DSP and Neural DSP does not support it. You use it at your own
risk. Make a backup of your presets before you start.

Terms in this guide:

- **unit**: your Quad Cortex.
- **computer**: the Linux or macOS computer that runs `scripts/install.sh`.
- **stock CorOS**: the Neural DSP software and UI on the unit (the program `ZenUI`).
- **Tetrad**: the program `tetrad`, which replaces the stock UI when it is selected.

## 1. Requirements

| Item | Requirement |
| --- | --- |
| Unit | A Neural DSP Quad Cortex. |
| CorOS | 4.1.0 or 4.1.1. On other builds Tetrad does not start and the unit starts stock CorOS. |
| Root SSH | You can log in to the unit as `root` over SSH from the computer. This guide does not tell you how to get this access. That is out of scope. |
| Init script | `/etc/init.d/S80drvinit` on the unit is the stock CorOS file (md5 `688ddb2805f34c69a8a1d7d173a406b7`), or that file with the Tetrad hook or the zen-rs hook. If it is different, the script stops and changes nothing. |
| Computer | Linux or macOS with `sh`, OpenSSH `ssh`, `od`, and `md5sum` or `md5`. `python3` only for `--proxy`. |
| Tetrad files | A release archive from [GitHub releases](https://github.com/JudahZF/tetrad/releases) (check it with its `.sha256` file), or your own build (see below). |
| Network | The unit and the computer are on the same network. The upload is about 15 MB and takes about one minute over Wi-Fi. |

The script uses these local files:

| File | Release archive | Your own build |
| --- | --- | --- |
| The binary | `bin/tetrad` | `dist/tetrad` (from `nix develop path:./qc-dev -c ./qc-dev/build-arm.sh . dist`) |
| The launcher scripts | `launcher/` or `scripts/launcher/`: `tetrad-launch.sh`, `tetrad-launcher.sh`, `tetrad-update.sh` | `scripts/launcher/` |

To use a binary at a different path, add `--binary PATH`. The binary must be the 32-bit ARM
build. The script stops if the file is not an ARM Linux binary.

## 2. Set up SSH

1. Find the IP address of the unit (for example in your router).
2. Make sure that this command works and prints `0`:

   ```sh
   ssh root@<unit-ip> id -u
   ```

   If your access uses a different port, add `-p <port>`.
3. Optional: add an alias to `~/.ssh/config`. Then you can use `--host qc`:

   ```text
   Host qc
       HostName <unit-ip>
       Port <port>
       User root
       IdentityFile ~/.ssh/<your-key>
   ```

4. Optional: set the default unit for the script, so you can omit `--host`:

   ```sh
   export QC_HOST=qc
   ```

The script logs in as `root` unless `HOST` has a user name (`user@host`). It opens one SSH
connection for all steps, so you type a password one time at most.

### SSH problems

| Problem | Cause | What to do |
| --- | --- | --- |
| The connection closes at once, before a password or key prompt | The unit runs OpenSSH 6.7. It can drop a modern client that sends its first line in one packet. | Add `--proxy` to each command. It sends the first line slowly through `scripts/qc-ssh-proxy.py` (needs `python3`). For plain `ssh`, add `ProxyCommand python3 /path/to/tetrad/scripts/qc-ssh-proxy.py %h %p` to the `Host` entry. |
| The key is refused, but it is correct | OpenSSH 6.7 does not know `rsa-sha2` signatures. New clients do not send `ssh-rsa` by default. | Use an `ed25519` key, or add `PubkeyAcceptedAlgorithms +ssh-rsa` to the `Host` entry. |
| `not root; log in as root` | You logged in as a different user. | Use `root@<unit-ip>` or `User root`. |
| A different ssh option is necessary | | Add `--ssh-opt 'Name=value'` (the script gives it to ssh as `-o Name=value`). You can repeat it. |

## 3. Install

1. Go to the Tetrad folder (the release archive or your clone).
2. Optional: see the commands that the script will run. This connects to nothing:

   ```sh
   scripts/install.sh --host qc --dry-run install
   ```

3. Install:

   ```sh
   scripts/install.sh --host qc install
   ```

The script does these steps. It stops at the first step that fails.

| Step | What the script does on the unit |
| --- | --- |
| 1. Check | Checks root login, `/opt/neuraldsp/ZenUI`, `/opt/neuraldsp/zenreboot`, `/media/p4` mounted, and the md5 of `S80drvinit`. Changes nothing. |
| 2. Stop | Stops `ZenWatchdog`, then the UI (SIGTERM, SIGKILL after 5 s). The screen does not change until the restart. Without this step, ZenWatchdog restarts the unit and the stock process killer can stop the upload. |
| 3. Upload | Makes `/media/p4/tetrad/bin` and `/media/p4/tetrad/launcher`. Writes each file as `<name>.part` and checks its md5 on the unit. Checks that the launcher scripts parse (`sh -n`). Runs `tetrad run --check`: exit 0 means that Tetrad supports the CorOS build on the unit. Only then it moves all files into place (`mv`). If a check fails, it removes the `.part` files and the old files stay. |
| 4. Hook | `tetrad-launcher.sh install-hook`: changes two lines of `/etc/init.d/S80drvinit` (see section 10). |
| 5. Select | `tetrad-launcher.sh arm`: selects this binary by its md5. |
| 6. Status | `tetrad-launcher.sh status`. |
| Restart | `/opt/neuraldsp/zenreboot`. |

If a step fails after step 2, the script also restarts the unit, so that a UI runs again. With
`--no-reboot` the script does not restart the unit. Then no UI runs until you restart it.

After the restart the boot selector shows for 5 seconds (section 6). If you do not touch the
screen, Tetrad starts. You can run `install` again on an installed unit. It replaces the files
and selects the new binary.

## 4. Check the status

```sh
scripts/install.sh --host qc status
```

This command changes nothing. Example output:

```text
hook:     hooked (S80drvinit md5 1a259e0f0031a179df6e8a0854731421)
select:   72d2edae... (bin/tetrad md5 72d2edae...)
chooser:  on (saved choice: tetrad)
running:  Tetrad
```

| Line | Meaning |
| --- | --- |
| `hook` | `hooked`: the boot hook is installed. `stock`: no hook; the unit always starts stock CorOS. `unknown`: `S80drvinit` is not a known file. |
| `select` | The md5 of the selected binary. `none`: Tetrad is turned off. If the two md5 values are different, the unit starts stock CorOS. Run `enable` to select the binary that is on the unit. |
| `chooser` | The boot selector is on or off, and the choice that starts after 5 seconds. |
| `disabled` | Shows only when the launcher turned Tetrad off, with the reason. The unit then starts stock CorOS until you run `enable`. |
| `pending` | Tetrad starts that did not finish start-up. After 3, the launcher turns Tetrad off. |
| `running` | The UI that runs now: `Tetrad`, `stock CorOS` or `none`. |
| last lines | The last decisions of the boot launcher (`launch.log`). |

## 5. Update Tetrad

From the touchscreen: Settings → Tetrad → Tetrad Updates (Settings → System on 0.1.0-alpha.8 and earlier), then CHECK TETRAD and UPDATE TETRAD. The
unit needs Wi-Fi. This does not change CorOS. See [updates.md](updates.md#tetrad-updates).

From the computer, when you have a new release or a new build:

```sh
scripts/install.sh --host qc update
```

The script stops ZenWatchdog and the UI, uploads and checks the new files as in section 3,
selects the new binary and restarts the unit. It does not change `S80drvinit`. Your boot
selector settings stay. The start counter and a `disabled` state are cleared, so the new
binary gets three new tries.

If `tetrad run --check` says that the CorOS build is not supported, the script replaces
nothing. The old Tetrad stays and the unit restarts.

### CorOS updates

| How you update CorOS | Result |
| --- | --- |
| In Tetrad: Settings, CorOS Updates, UPDATE COROS | The new CorOS keeps the boot hook and your SSH keys. The first boot runs stock CorOS one time, so it can make its factory files. Then the unit restarts by itself and Tetrad starts. |
| In stock CorOS | The new CorOS has no boot hook. The unit starts stock CorOS. Your files in `/media/p4/tetrad` stay. Your SSH keys can be gone after this update; then get root SSH access again. Run `scripts/install.sh enable` to add the hook again. |
| Tetrad refuses the update (error code 220) | Run `disable`, restart, update in stock CorOS, then run `enable`. |

If Tetrad does not support the new CorOS build, the unit starts stock CorOS until a Tetrad
release supports that build. Then install it with `update`.

## 6. The boot selector

When Tetrad is selected and can start, the unit shows a screen with two buttons, **CorOS** and
**Tetrad**, for 5 seconds.

- Touch a button to start that UI now.
- If you do not touch the screen, the marked button starts. This is your last choice. Before
  the first choice it is Tetrad.
- A touch on CorOS does not count as a failed Tetrad start.

To start Tetrad with no selector, or to get the selector back, use the **Show at boot** switch in
Settings → Tetrad → Boot Selector. Or run on the unit:

```sh
ssh root@<unit-ip> sh /media/p4/tetrad/launcher/tetrad-launcher.sh chooser off
ssh root@<unit-ip> sh /media/p4/tetrad/launcher/tetrad-launcher.sh chooser on
```

## 7. Turn Tetrad off or on

```sh
scripts/install.sh --host qc disable
scripts/install.sh --host qc enable
```

| Command | What it does on the unit |
| --- | --- |
| `disable` | `tetrad-launcher.sh disarm`: removes the selection, then restarts. The unit starts stock CorOS on each boot. The hook and the files stay. |
| `enable` | `tetrad-launcher.sh install-hook` (only if the hook is not there), then `arm`: selects the binary in `/media/p4/tetrad/bin` and clears `disabled` and the start counter. Then it restarts. |

Add `--no-reboot` to apply the change at the next restart.

## 8. Uninstall

```sh
scripts/install.sh --host qc uninstall
```

The script does these steps:

1. `tetrad-launcher.sh disarm`: stock CorOS from the next boot on.
2. `tetrad-launcher.sh remove-hook`: `S80drvinit` becomes the stock file again, byte for byte (md5
   `688ddb2805f34c69a8a1d7d173a406b7`).
3. It asks if it must also delete `/media/p4/tetrad` (binary, launcher, logs, boot choice).
   Add `--yes` to delete with no question. With no answer the folder stays. Without the hook,
   nothing runs the files in it.
4. It restarts the unit.

## 9. Recovery

The design keeps stock CorOS as the fall-back. Use the first row that applies.

| Problem | What to do |
| --- | --- |
| Tetrad does not work as you want, the screen works | At the next boot, touch **CorOS** on the boot selector. Then run `scripts/install.sh disable`. |
| Tetrad crashes or hangs before its UI shows | Do nothing. Each failed start counts. After 3 failed starts in a row, the next boot starts stock CorOS and the launcher writes `disabled`. Run `status` to see the reason. |
| Tetrad hangs after its UI shows, and there is no boot selector (`chooser off`) | Turn the unit off during the Tetrad start-up (before its UI shows), 3 times in a row. Each counts as a failed start. The 4th boot starts stock CorOS. |
| Tetrad runs and SSH works | `scripts/install.sh disable`, or `uninstall`. |
| The unit starts stock CorOS, but you expect Tetrad | Run `status`. `hook: stock` (for example after a CorOS update in stock CorOS): run `enable`. `disabled: ...`: fix the cause, then run `enable`. `select` md5 different from `bin/tetrad`: run `enable`. |
| The launcher file is missing or broken | Nothing is necessary. The hook starts stock CorOS when `/media/p4/tetrad/launcher/tetrad-launch.sh` is missing, `/media/p4` is not mounted, or the file does not parse. |
| No SSH, no usable screen | You need the storage card of the unit in a computer. On partition 4 (ext4), delete `tetrad/launcher/select`. The unit then starts stock CorOS. To remove the hook too, mount the active root partition and run `TD_ROOT=/path/to/root sh tetrad-launcher.sh remove-hook`. |

The script never writes `/opt/neuraldsp`, the boot loader or a partition table. Stock CorOS
stays on the unit and is never changed.

## 10. What the script changes on the unit

| Path | Change | Undo |
| --- | --- | --- |
| `/media/p4/tetrad/bin/tetrad` | The Tetrad binary. | `uninstall --yes` |
| `/media/p4/tetrad/launcher/tetrad-launch.sh` | The boot launcher. It picks the UI of each boot. | `uninstall --yes` |
| `/media/p4/tetrad/launcher/tetrad-launcher.sh` | The tool for `status`, `arm`, `disarm`, `chooser`, `install-hook`, `remove-hook`. | `uninstall --yes` |
| `/media/p4/tetrad/launcher/tetrad-update.sh` | The CorOS installer that Tetrad runs for CorOS Updates. It keeps the hook and your SSH keys on the new CorOS. | `uninstall --yes` |
| `/media/p4/tetrad/launcher/select` | The md5 of the selected binary (written by `arm`). | `disable` |
| `/etc/init.d/S80drvinit` | Two lines: in place of `./ZenUI`, init runs `sh tetrad-launch.sh` if that file exists and parses. The tool changes the file only when its md5 is a known value, writes the new file beside it, checks the md5, and then renames it into place. | `uninstall` (or `remove-hook`) |

At run time the launcher and Tetrad also write these files in `/media/p4/tetrad/launcher`:
`pending` (start counter), `disabled` (reason, when the launcher turned Tetrad off),
`boot-choice` (last selector choice), `no-chooser` (`chooser off`) and `launch.log`.

The script does not change:

- `/opt/neuraldsp` (stock CorOS) and your presets.
- A partition, the boot loader or the CorOS slot marker. Only a CorOS update that you start in
  Tetrad writes the other root partition, as stock CorOS does for an update.

## 11. Messages

| Message | Meaning | What to do |
| --- | --- | --- |
| `S80drvinit is not a known CorOS 4.1.0 or 4.1.1 file; refused` | The init script was changed by something else. | Put back the stock file, then run `install` again. |
| `this Tetrad does not support the CorOS build on the unit` | `tetrad run --check` exited 4. Nothing was replaced. | Use a Tetrad release that supports your CorOS build. |
| `... has md5 ..., not ...; removed` | The upload was damaged. Nothing was replaced. | Run the command again. |
| `cannot write ... (is /media/p4 full?)` | The upload could not be written. | Make free space on `/media/p4`, then run the command again. |
| `the binary does not start on the unit` | The file is not the right ARM build. | Use the release binary or an `armv7-unknown-linux-gnueabihf` release build. |
| `Tetrad is not installed` | `update`, `disable`, `enable` or `uninstall` found no launcher. | Run `install`. |
| `the SSH connection failed` | SSH did not connect, or the connection was lost. | See section 2. |

## Migrating from zen-rs

zen-rs is the old working name of Tetrad. A unit with zen-rs has the folder `/media/p4/zenui-rs`
and the zen-rs hook in `S80drvinit` (md5 `d6bafded21b2192dc18927f575fbb742`). You do not have to
remove zen-rs first. Run `update` (or `install`) with this Tetrad release:

```sh
scripts/install.sh --host qc update
```

When the script finds `/media/p4/zenui-rs`, it does these steps after it stops ZenWatchdog and
the UI:

1. It removes the zen-rs hook with the zen-rs launcher that is on the unit
   (`sh /media/p4/zenui-rs/launcher/zr-launcher.sh remove-hook`). Then it checks that
   `S80drvinit` is the stock file (md5 `688ddb2805f34c69a8a1d7d173a406b7`).
2. It renames `/media/p4/zenui-rs` to `/media/p4/tetrad`.
3. It deletes the zen-rs binary, the zen-rs scripts, `select` and `pending`. The logs,
   `boot-choice`, `no-chooser` and `launch.log` stay.

Then it uploads Tetrad, adds the Tetrad hook, selects the new binary and restarts the unit, as in
section 3. Your boot selector settings stay. A saved choice `zen-rs` starts Tetrad.

At its first start, Tetrad also renames the data that zen-rs wrote in the stock folders:

- In `zenui.db`, the table `zen_capture_quarantine` (the Corrupt Captures list) becomes
  `tetrad_capture_quarantine`. If both tables exist, Tetrad copies the rows that are only in
  the old table, then deletes the old table.
- In `/media/p4/Captures/My Captures`, the staged copies `*.zen-local` and `*.zen-converted`
  become `*.tetrad-local` and `*.tetrad-converted`. If the new name exists, Tetrad keeps both
  files and writes a notice. Temporary files `*.zen-tmp-*` are crash leftovers: Tetrad deletes
  them there and when it deletes a setlist.

Tetrad does not change stock tables or stock files.

If a step fails, the script stops and restarts the unit. The next boot starts stock CorOS, or
zen-rs if the old hook is still there. Then run `scripts/install.sh --host qc install`: it
continues from each of these states.

| Message | Meaning | What to do |
| --- | --- | --- |
| `/media/p4/zenui-rs and /media/p4/tetrad both exist` | The unit has two installations. | Delete the folder that you do not need, then run the command again. |
| `S80drvinit is not the stock file after remove-hook` | The zen-rs launcher did not give back the stock file. Nothing was moved. | Run `sh /media/p4/zenui-rs/launcher/zr-launcher.sh status` on the unit and check the file. |
| `Tetrad is not installed`, then `/media/p4/zenui-rs is zen-rs` | `status`, `disable`, `enable` or `uninstall` found zen-rs. | Run `update` first. |
