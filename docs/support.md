# Support file

The support file is a `.zip` with what we need to look into a bug report: versions, licence
states, the blocks of the loaded preset, extensions and recent logs. Tetrad never sends it
anywhere. You download it and send it with your report.

## Get the support file

1. Connect the unit to Wi-Fi (**Settings → System → Connection**).
2. Open **Settings → Support → Send Report**. Turn on **Remote** if it is off. The page shows
   the address, for example `http://192.168.1.20`, and a 4-digit PIN.
3. On a computer or phone on the same network, open the address in a browser and enter the PIN.
4. Under **Support file**, tap **Download**. The browser saves
   `tetrad-support-<date>-<time>.zip` (UTC).

The file is usually smaller than 100 KB. See [remote.md](remote.md) for the Remote page and its
PIN.

From a computer you can also use `curl`:

```sh
curl -H 'x-tetrad-pin: 4821' -o support.zip http://192.168.1.20/api/support
```

## What is in it

| File | Content |
| --- | --- |
| `summary.txt` | Tetrad version and binary md5, CorOS build, unit type, a unit id (see below), kernel and U-Boot versions, boot number, uptime, Wi-Fi state, online, signed in, Cortex Control connected, free storage, DSP fault, loaded preset path |
| `licences.txt` | Each plugin id: Unlicensed, Inactive or Active, its model count and one model name; the activation bit; the stored and new plugin ids; whether licence data is stored (not the data) |
| `catalog.txt` | Model counts: all, hidden, internal, plugin and extension models |
| `preset.txt` | Path, name and scene of the loaded preset; each block's row, column, model id, plugin id, bypass and name. No parameter values |
| `extensions.txt` | Installed extensions with version, source and state, their DSP blocks (category and CPU, or why a block is off) and their 40 newest log lines. No extension settings |
| `dsp.txt` | DSP load, and per core the CPU, heap use, dropped messages and FP exceptions (as DSP Diagnostics shows them) |
| `notices.txt` | The last 256 notices (warnings and errors) of this boot, with the time of day once the clock is set |
| `logs/boot-<n>.log` | The 5 newest stock boot logs (this boot and the 4 before it), the last 256 KiB of each. `n` is the log's own number |
| `logs/launch.log`, `logs/launch.log.1` | The boot launcher log: boot choices, unconfirmed starts and why Tetrad was turned off |

## What is never in it

- Cloud and Cortex Control sign-in tokens, account names and email.
- The plugin licence data (`PluginLicenses.ini`), activation codes and signing material.
- Wi-Fi network names and passwords, the Remote PIN.
- The serial number. `summary.txt` has a unit id instead: the first 8 hex digits of the SHA-256
  of the serial. It tells us that two reports come from the same unit. The stock boot log
  prints the serial; each copy becomes `<serial>`.
- Presets, Neural Captures, IRs and parameter values. `preset.txt` lists block models only.

The logs go in as they are. Tetrad's own lines carry no secrets. Extensions also write to the
boot log and to `extensions.txt`, and an extension can write any text. If you use an extension
that may print something private, look at those files before you send the zip.
