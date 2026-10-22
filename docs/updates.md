# Updates on a unit with Tetrad

Tetrad has two update pages in Settings → System, one below the other. They update different
things:

| Page | Button | What it updates | What stays |
| --- | --- | --- | --- |
| **Tetrad Updates** | UPDATE TETRAD (blue) | Tetrad: its binary and launcher scripts on p4 | CorOS, the boot hook, your presets |
| **CorOS Updates** | DOWNLOAD COROS, UPDATE COROS (black) | CorOS: the other root partition | Tetrad, the boot hook, your SSH keys |

See [Tetrad updates](#tetrad-updates) for the Tetrad page. The rest of this file is about CorOS.

Tetrad can download and install a CorOS update on the unit, with no computer. The install keeps
the Tetrad boot hook and your SSH keys on the new CorOS.

## How a stock update works

The unit has two root partitions, p1 and p2. One runs, and the other is free. The boot partition
p3 holds one empty file, `one` or `two`, that selects the root partition for the next boot.

A stock install runs the running root's `/opt/neuraldsp/sysupdate_run.sh`. The script:

1. checks the package with `gzip -t` (there is no signature check),
2. formats the free root partition,
3. copies the package root file system into it,
4. converts the per-device encrypted files for this unit,
5. copies the kernel and device trees,
6. flips the p3 file to the new root partition (the commit point),
7. writes progress tokens (`UPDATER_START`, `UPDATER_STEP_1` to `UPDATER_STEP_15`,
   `UPDATER_COMPLETE`, `UPDATER_FAILURE`) to `/etc/updater_status`.

The user data partition (p4) stays. A stock update loses every change to the root file system:
the boot hook and any SSH key that you added.

## What Tetrad does

In Tetrad, Settings → System → CorOS Updates works as stock: CHECK, DOWNLOAD COROS and UPDATE
COROS (stock: DOWNLOAD UPDATE and UPDATE). Cortex Control can also start the install.

1. **Version check.** Tetrad reads the offered version from the first line of the release notes.
   When Tetrad has no complete firmware profile for that version, UPDATE COROS first asks: "Tetrad does
   not support CorOS X yet. The update keeps Tetrad, but the Quad Cortex starts stock CorOS until
   Tetrad supports CorOS X." CANCEL sends nothing. INSTALL continues. Cortex Control does not
   ask.
2. **Stock steps.** Tetrad mutes the outputs, locks Cortex Control and masks the local inputs, as
   stock does.
3. **The installer.** Tetrad runs `/bin/sh /media/p4/tetrad/launcher/tetrad-update.sh` in
   `/opt/neuraldsp`, in place of the stock script. When `tetrad-update.sh` is missing, Tetrad refuses
   the install. It never falls back to the stock script, because that would drop the hook.
4. **Progress.** The stock script writes the same tokens, so the progress bar works as stock.
5. **End.** On success Tetrad restarts the unit at once. On a failure it shows the stock failure
   page, unmutes and unlocks.

### `tetrad-update.sh`

`tetrad-update.sh` first checks, and writes nothing when a check fails (exit 220):

- it runs as root,
- the running `sysupdate_run.sh` has the pinned md5 (the script of CorOS 4.0.1, 4.1.0 and 4.1.1),
- the script has exactly one `UPDATER_STEP_13` line,
- `tetrad-launcher.sh` exists and parses,
- the package `/media/p4/UpdaterTemp/update.bin` passes `gzip -t`.

Then it makes a copy of the stock script in `/tmp` with one added line before step 13 (the p3
flip). The stock script itself does not change. At the added line the new root is written and
still mounted. The added step:

1. adds the boot hook to the new root with `tetrad-launcher.sh install-hook` and reads the result back
   (md5),
2. adds to the new root's `/root/.ssh/authorized_keys` each key line of the running root that it
   does not have, with mode 0600, and reads it back,
3. syncs, unmounts the new root and checks it with `e2fsck -fn`.

Only then does the stock script flip p3. When the added step fails, it cleans up as the stock
failure branches do and exits 218 before the flip. The old root then boots again.

### Exit codes

| Code | Meaning | p3 marker |
| --- | --- | --- |
| 0, then `UPDATER_COMPLETE` | Installed. The new root has the hook and your keys | flipped |
| 1 | Stock: the per-device conversion failed | unchanged |
| 201–217 | Stock failures (mount, unpack, format, copy) | unchanged |
| 218 | The added step failed (hook, keys, unmount or `e2fsck`) | unchanged |
| 220 | Refused before the stock script started. Nothing was written | unchanged |

The system log has the reason for 218 and 220.

## First boot of the new CorOS

The first boot of the new root starts stock ZenUI once, because stock must make its factory
files first. A watcher then restarts the unit, and the next boot starts Tetrad. See
[launcher.md](launcher.md#first-boot-after-a-coros-update).

When the new CorOS is not supported, `tetrad run --check` exits 4. The launcher then writes
`disabled`, and the unit starts stock CorOS until a Tetrad build that supports it is uploaded and
armed.

## After an update

- The new root has its own SSH host keys. Your SSH client warns about a changed host key. Add the
  new key to your `known_hosts`.
- A stock CorOS first start can migrate the library database (`zenui.db`). Tetrad writes only the
  schema of CorOS 4.1.x (410.1).
- Tetrad files on p4 stay: the binary, the launcher, `select`, the boot choice and the logs.

## Roll back

The previous CorOS stays on the other root partition until the next update formats it. To boot it
again, change the p3 file to the other name and restart. For example, over SSH:

```sh
mkdir -p /tmp/p3 && mount /dev/mmcblk0p3 /tmp/p3
ls /tmp/p3                          # shows "one" or "two"
rm /tmp/p3/one && touch /tmp/p3/two # or the other way
sync && umount /tmp/p3
/opt/neuraldsp/zenreboot
```

You can also do this with the SD card in a computer. This is a write to the boot partition. The
project has not tested a roll back or this example on a unit yet. Check each step first. No automatic fall-back to the other root is
known: when a new root does not boot, only this manual step brings the old one back.

Tested: the full CorOS 4.1.0 to 4.1.1 update from the touchscreen on one unit, and the installer
in a QEMU virtual machine with a synthetic package and the real 4.1.1 package. Bring-up runs
(`tetrad bringup`) refuse every update download and install.

## Tetrad updates

Settings → System → Tetrad Updates updates Tetrad from its GitHub releases, with no computer.
CorOS does not change. The page shows the running Tetrad version.

1. **CHECK TETRAD.** Tetrad reads the release list of `JudahZF/tetrad` from the GitHub API. It
   takes the newest release that is newer than the running version and has the asset
   `tetrad-update.json`. A stable Tetrad takes only stable releases. A pre-release Tetrad also
   takes pre-releases. Drafts do not count.
2. **The manifest.** `tetrad-update.json` gives the version and, for each of the four files
   (`bin/tetrad` and the three launcher scripts), its size and SHA-256. Tetrad first checks
   `tetrad-update.json.sig`, the Ed25519 signature of the manifest, against the release key that
   is built into Tetrad (`RELEASE_KEY` in `crates/tetrad-services/src/updater/tetrad.rs`). It
   refuses a manifest with a bad signature, another version or another list of files.
3. **UPDATE TETRAD.** Tetrad does the steps of `scripts/install.sh update` on the unit:
   1. It downloads each file over HTTPS from the release to `/media/p4/tetrad/<path>.part`, and
      checks its size and SHA-256. The progress bar shows the binary download.
   2. It checks that the scripts parse (`sh -n`) and that the new binary supports the running
      CorOS (`tetrad run --check` exits 0).
   3. It moves the files into place and runs `tetrad-launcher.sh arm`. The new binary gets three
      new starts, as after `install.sh update`.
   4. It restarts the unit.

When a check in steps 1 or 2 fails, Tetrad deletes the `.part` files and the old Tetrad stays.
The page shows the reason, and UPDATE TETRAD tries again. When `arm` fails after the move, the
md5 in `select` is the old one, so the unit starts stock CorOS until `tetrad-launcher.sh arm` runs.

A Tetrad update and a CorOS update never run at the same time. Bring-up runs (`tetrad bringup`)
refuse UPDATE TETRAD.

The release workflow (`.github/workflows/release.yml`) publishes `tetrad-update.json`, its
signature and the three scripts with each release. It signs with the repository secret
`TETRAD_RELEASE_KEY` (an Ed25519 key in PEM). It stops when the tag is not `v` and the version
in `Cargo.toml`, when the secret is missing, or when the secret is not the key that
`RELEASE_KEY` pins. Releases up to 0.1.0-alpha.3 have no manifest and no Tetrad Updates page:
install the first release with this page with `scripts/install.sh update`.

Trust: the signed manifest pins the SHA-256 of each file, so a release asset that was changed
after the build does not install. The private key is only in the repository secret, so a person
who can change the release workflow can also sign. To change the key: put the new public key in
`RELEASE_KEY` and the new private key in the secret. Units with the old key refuse the new
releases, so install the first release with the new key with `scripts/install.sh update`.

Tested: on the host, with scripted HTTP and commands. Not tested on a unit yet.
