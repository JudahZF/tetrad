#!/bin/sh
# tetrad-launch.sh: the Tetrad boot launcher (BusyBox ash 1.23). Design and recovery:
# docs/launcher.md.
#
# With the init hook (`tetrad-launcher.sh install-hook`), S80drvinit runs this file in place of
# `./ZenUI`: cwd /opt/neuraldsp, the tslib and QC_SERIAL_NUMBER variables set, stdout and
# stderr piped into the system log. It picks the UI of this boot:
#   stock ZenUI  no `select` file; a `disabled` file; bin/tetrad missing or not the md5 in
#                `select`; TD_MAX_STARTS tetrad starts in a row that never confirmed
#                start-up; CorOS picked on the boot selector (or saved there, when the
#                selector fails); tetrad exit 4 (stock firmware not a supported CorOS
#                build); the first boot after a CorOS update (below).
#   tetrad       otherwise: `tetrad run --started launcher/pending`. tetrad removes
#                `pending` once start-up ends with the DSP up (main.rs `run_device`).
# The boot selector (`tetrad select`, docs/launcher.md) runs when Tetrad
# could start, unless a `no-chooser` file exists (then Tetrad starts, as before the selector).
# First boot after a CorOS update (docs/updates.md): a new root has none of
# the factory files that stock ZenUI makes at its first start and Tetrad only reads. Then stock
# starts (no selector), and a watcher (`tetrad-launch.sh first-boot-watch`) restarts the unit
# once stock has made them, so the next boot starts Tetrad.
# Stock starts as `exec ./ZenUI`: the same process init starts without the launcher.
# Writes only in /media/p4/tetrad/launcher; never /opt/neuraldsp or a partition.
# Sourcing the file defines the functions and runs nothing (host tests set TD_ROOT).

TD_ROOT=${TD_ROOT:-}
TD_HOME=$TD_ROOT/media/p4/tetrad/launcher
TD_BIN=$TD_ROOT/media/p4/tetrad/bin/tetrad
TD_LOG=$TD_HOME/launch.log
# Starts in a row that end before tetrad confirms start-up (crash, hang, power cut, no
# DSP); the next boot after the last one runs stock and writes `disabled`.
TD_MAX_STARTS=3
# tetrad_app::device::linux::WRONG_FIRMWARE_EXIT (a host test compares them).
TD_WRONG_FIRMWARE=4
# `tetrad select` exit status: tetrad_app::boot_select::{STOCK_EXIT, TETRAD_EXIT} (a host test
# compares them). Every other status is a selector failure.
TD_SELECT_STOCK=10
TD_SELECT_TETRAD=11
# The saved choice, `coros` or `tetrad`; Tetrad when missing. The selector writes it.
TD_CHOICE=$TD_HOME/boot-choice
# Seconds after which the launcher kills the selector: its 5 s countdown, start and margin.
TD_SELECT_LIMIT=${TD_SELECT_LIMIT:-8}
# The first-boot files in /opt/neuraldsp. Tetrad reads FACTORY and never writes them
# (factory.db: device/boot.rs; factory_ir_entities.pb: tetrad-media catalog.rs;
# searchIndexFactory: tetrad-library stock/search.rs). Stock ZenUI makes them at its first start;
# on the 4.1.0 root factory_ir_entities.pb is the last one (mtime 01:04:13). DONE is stock's
# capture conversion marker (01:00:42); Tetrad runs the conversion itself when it is missing.
TD_FACTORY='factory.db factory_ir_entities.pb searchIndexFactory'
TD_FIRST_BOOT_DONE=captureConversionFinished
# The first-boot watcher, in seconds: it gives up after CAP s of waiting for stock, checks
# every POLL s, and restarts SETTLE s after the files are there.
TD_FIRST_BOOT_CAP=${TD_FIRST_BOOT_CAP:-1200}
TD_FIRST_BOOT_POLL=${TD_FIRST_BOOT_POLL:-10}
TD_FIRST_BOOT_SETTLE=${TD_FIRST_BOOT_SETTLE:-90}
TD_REBOOT=${TD_REBOOT:-$TD_ROOT/opt/neuraldsp/zenreboot}
TD_UPDATER_STATUS=$TD_ROOT/etc/updater_status

# One line to stdout (the system log) and to launch.log (cut to the newest 32 KiB).
td_log() {
    echo "tetrad: $*"
    [ -d "$TD_HOME" ] || return 0
    if [ -f "$TD_LOG" ] && [ "$(wc -c <"$TD_LOG")" -gt 32768 ]; then
        mv -f "$TD_LOG" "$TD_LOG.1"
    fi
    echo "$(date '+%Y-%m-%d %H:%M:%S') $*" >>"$TD_LOG" 2>/dev/null
}

# Names this shell for ZenWatchdog's `pidof ZenUI` (docs/launcher.md). No-op off the QC (host tests).
td_comm() {
    { printf '%s' "$1" >"$TD_ROOT/proc/$$/comm"; } 2>/dev/null
}

# Stock ZenUI replaces this shell, as init starts it without the launcher.
td_stock() {
    td_log "stock ZenUI: $*"
    exec ./ZenUI
}

# Stock on every boot until a person runs `tetrad-launcher.sh arm` again.
td_disable() {
    echo "$*" >"$TD_HOME/disabled"
    sync
}

# The boot selector: CorOS execs stock here, Tetrad returns. A selector that fails, or runs
# past TD_SELECT_LIMIT s and is killed, gives the saved choice without a screen. This shell
# stays "ZenUI" for ZenWatchdog while the selector runs (the selector renames itself too).
td_choose() {
    [ -f "$TD_HOME/no-chooser" ] && return 0
    "$TD_BIN" select --file "$TD_CHOICE" &
    select_pid=$!
    # BusyBox 1.23.1 has no `timeout`. A killed watcher leaves its `sleep` to end by itself;
    # the sleep holds no log pipe, so nothing waits for it.
    ( sleep "$TD_SELECT_LIMIT"; kill -9 "$select_pid" ) >/dev/null 2>&1 &
    watcher_pid=$!
    wait "$select_pid"
    select_code=$?
    kill "$watcher_pid" 2>/dev/null
    case $select_code in
        "$TD_SELECT_TETRAD") td_log "boot selector: Tetrad" ;;
        "$TD_SELECT_STOCK") td_stock "chosen at boot" ;;
        *)
            [ "$(cat "$TD_CHOICE" 2>/dev/null)" = coros ] &&
                td_stock "boot selector exit $select_code; the saved choice is CorOS"
            td_log "boot selector exit $select_code; the saved choice is Tetrad"
            ;;
    esac
}

# The factory files that Tetrad reads are in /opt/neuraldsp (not empty).
td_factory_ready() {
    for name in $TD_FACTORY; do
        [ -s "$TD_ROOT/opt/neuraldsp/$name" ] || return 1
    done
}

# Stock's first start is done: the factory files and its last marker are there.
td_first_boot_done() {
    td_factory_ready && [ -e "$TD_ROOT/opt/neuraldsp/$TD_FIRST_BOOT_DONE" ]
}

# No CorOS install runs: /etc/updater_status (S80drvinit removes it at boot) holds no START or
# step token (tetrad-services updater/install.rs).
td_no_install() {
    case $(cat "$TD_UPDATER_STATUS" 2>/dev/null) in
        UPDATER_START | UPDATER_STEP_*) return 1 ;;
    esac
}

# Polls "$@" every TD_FIRST_BOOT_POLL s until it succeeds; fails when td_left (s) is used up.
td_wait_for() {
    until "$@"; do
        [ "$td_left" -gt 0 ] || return 1
        sleep "$TD_FIRST_BOOT_POLL"
        td_left=$((td_left - TD_FIRST_BOOT_POLL))
    done
}

# The first-boot watcher: `sh tetrad-launch.sh first-boot-watch`, a process of its own (comm `sh`,
# so `pidof ZenUI` never counts it) with no log pipe. It restarts the unit once stock has made
# the first-boot files, so the next boot starts Tetrad, but never while a CorOS install runs.
# After TD_FIRST_BOOT_CAP s of waiting it gives up; a restart by hand then does the same.
td_first_boot_watch() {
    td_left=$TD_FIRST_BOOT_CAP
    td_wait_for td_first_boot_done || {
        td_log "first boot: no factory files after $TD_FIRST_BOOT_CAP s; no restart"
        return 0
    }
    td_log "first boot: stock made the factory files; restart in $TD_FIRST_BOOT_SETTLE s"
    sleep "$TD_FIRST_BOOT_SETTLE"
    td_wait_for td_no_install || {
        td_log "first boot: a CorOS install runs; no restart"
        return 0
    }
    td_log "first boot done: restart; the next boot starts Tetrad"
    sync
    exec "$TD_REBOOT"
}

# The first boot of a new root: stock starts and makes the factory files. `tetrad run
# --check` (it opens nothing) first tells if Tetrad supports the new build: if it does, the
# watcher starts; if not, the exit-4 rule applies now, so no restart comes for nothing.
td_first_boot() {
    td_log "first boot after an update: stock builds the factory files"
    "$TD_BIN" run --check
    code=$?
    case $code in
        0)
            sh "$TD_HOME/tetrad-launch.sh" first-boot-watch </dev/null >/dev/null 2>&1 &
            td_stock "first boot after an update; a restart follows when stock is done"
            ;;
        "$TD_WRONG_FIRMWARE")
            td_disable "stock firmware is not a supported CorOS build"
            td_stock "tetrad run --check exit $code: stock firmware is not a supported CorOS build"
            ;;
        *) td_stock "tetrad run --check exit $code; no restart after the first boot" ;;
    esac
}

td_launch() {
    # ZenWatchdog starts with this shell and reboots after 3 s without a ZenUI; the md5 below
    # alone takes ~1.8 s cold. The shell is "ZenUI" until it starts Tetrad, which renames itself
    # first thing; one ZenUI at a time, so a dead Tetrad still fails `pidof`.
    td_comm ZenUI
    cd "$TD_ROOT/opt/neuraldsp" || return 1
    [ -f "$TD_HOME/select" ] || td_stock "tetrad is not selected"
    [ -f "$TD_HOME/disabled" ] && td_stock "disabled: $(cat "$TD_HOME/disabled")"
    [ -x "$TD_BIN" ] || td_stock "$TD_BIN is missing or not executable"
    want=$(cut -c1-32 <"$TD_HOME/select")
    have=$(md5sum <"$TD_BIN" | cut -c1-32)
    [ "$have" = "$want" ] || td_stock "tetrad md5 $have is not the selected $want"
    starts=0
    [ -f "$TD_HOME/pending" ] && starts=$(cat "$TD_HOME/pending")
    case $starts in
        '' | *[!0-9]*) starts=$TD_MAX_STARTS ;;
    esac
    if [ "$starts" -ge "$TD_MAX_STARTS" ]; then
        td_disable "$starts tetrad starts in a row ended before start-up"
        td_stock "$starts tetrad starts in a row ended before start-up"
    fi
    td_factory_ready || td_first_boot
    td_choose
    starts=$((starts + 1))
    { echo "$starts" >"$TD_HOME/pending.tmp" && mv -f "$TD_HOME/pending.tmp" "$TD_HOME/pending" &&
        sync; } || td_stock "cannot write $TD_HOME/pending"
    td_log "tetrad run, unconfirmed start $starts of $TD_MAX_STARTS"
    td_comm tetrad-launch
    "$TD_BIN" run --started "$TD_HOME/pending"
    code=$?
    case $code in
        "$TD_WRONG_FIRMWARE")
            td_disable "stock firmware is not a supported CorOS build"
            td_stock "tetrad exit $code: stock firmware is not a supported CorOS build"
            ;;
        126 | 127) td_stock "tetrad could not start (exit $code)" ;;
    esac
    # 0: a power action (the stock teardown runs). Other codes: a failure; ZenWatchdog
    # reboots the unit about 8 s after `pidof ZenUI` fails.
    td_log "tetrad exit $code"
}

case ${0##*/} in
    tetrad-launch.sh)
        case ${1:-} in
            first-boot-watch) td_first_boot_watch ;;
            *) td_launch "$@" ;;
        esac
        exit $?
        ;;
esac
