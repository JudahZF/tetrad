#!/bin/sh
# tetrad-stage.sh STAGE SECONDS [TETRAD BRINGUP ARGS]
# One tetrad bring-up stage on the QC (BusyBox ash 1.23). Developer tool.
#
# Runs only when executed (`sh /media/p4/tetrad/bin/tetrad-stage.sh probe 120`); sourcing
# the file defines a function and runs nothing. It prints its plan, lets tetrad check
# the options (`bringup --check`, nothing opens), then stops stock ZenUI and starts
# tetrad in one step (the stock process killer lives in stock ZenUI, so the window must
# be short). Stock gets SIGTERM first: its own handler quits the Qt loop, so a write in
# progress finishes; SIGKILL only after 5 s. ZenWatchdog stays: when tetrad exits, the
# unit reboots to stock (R1). It never writes /opt/neuraldsp and never cd's into /media/p4.
# With `--power stock` (S10) the binary runs from a checked copy in /tmp.

TD_DIR=/media/p4/tetrad
TD_STOP=/tmp/tetrad.stop
TD_STOCK_EXE=/opt/neuraldsp/ZenUI
# Tenths of a second stock may take to quit after SIGTERM.
TD_STOCK_WAIT=50

# Review 2 R8: `pidof ZenUI` can also find a tetrad left from an earlier stage. Prints the
# pids whose /proc/PID/exe is stock ZenUI; returns 1 when another process has that name.
# BusyBox has no readlink applet here, so the link target comes from `ls -l`.
td_stock_pids() {
    other=0
    for pid in $(pidof ZenUI); do
        case $(ls -l "/proc/$pid/exe" 2>/dev/null) in
            *" -> $TD_STOCK_EXE") echo "$pid" ;;
            *) echo "tetrad-stage: pid $pid is named ZenUI but is not running $TD_STOCK_EXE (exited, replaced or another program)" >&2; other=1 ;;
        esac
    done
    return $other
}

td_stage() {
    if [ $# -lt 2 ]; then
        echo "usage: sh $TD_DIR/bin/tetrad-stage.sh probe|dsp|deploy|full SECONDS [tetrad bringup args]" >&2
        return 2
    fi
    stage=$1
    seconds=$2
    shift 2
    case $stage in
        probe|dsp|deploy|full) ;;
        *) echo "tetrad-stage: unknown stage '$stage'" >&2; return 2 ;;
    esac
    case $seconds in
        ''|*[!0-9]*) echo "tetrad-stage: SECONDS must be a whole number" >&2; return 2 ;;
    esac
    if [ ! -x "$TD_DIR/bin/tetrad" ]; then
        echo "tetrad-stage: $TD_DIR/bin/tetrad is missing or not executable" >&2
        return 1
    fi
    watchdog=$(pidof ZenWatchdog)
    if [ -z "$watchdog" ]; then
        echo "tetrad-stage: ZenWatchdog is not running, so nothing restores stock (R1). Refused." >&2
        return 1
    fi
    if ! stock=$(td_stock_pids); then
        echo "tetrad-stage: reboot to stock (R1) before the next stage. Refused." >&2
        return 1
    fi
    # Init's umask (files 0644, folders 0755) for the log folder made here; an SSH shell has
    # 077. tetrad sets 022 itself (gap census r48 A2).
    umask 022
    log=$TD_DIR/logs/$(date +%Y%m%d-%H%M%S)-$stage
    # S10 (review B16): the stock teardown (`--power stock`) unmounts /media/p4, so the binary
    # runs from tmpfs and holds no file there. The log folder stays on /media/p4: its open
    # journals still keep the mount busy (expected: read-only remount, Policy::Safe).
    bin=$TD_DIR/bin/tetrad
    prev=
    for arg in "$@"; do
        case "$prev $arg" in
            "--power stock"|*" --power=stock") bin=/tmp/tetrad ;;
        esac
        prev=$arg
    done

    echo "tetrad-stage: stage $stage, time limit $seconds s, log folder $log"
    echo "tetrad-stage: checks the options, stops stock ZenUI (pid ${stock:-none}; SIGTERM, SIGKILL after 5 s), then starts:"
    if [ "$bin" != "$TD_DIR/bin/tetrad" ]; then
        echo "tetrad-stage: --power stock: copies the binary to $bin (md5 checked) and runs it there"
    fi
    echo "  $bin bringup --stage $stage --time-limit $seconds --log-dir $log $*"
    echo "tetrad-stage: ZenWatchdog (pid $watchdog) stays; the unit reboots to stock after tetrad exits."
    echo "tetrad-stage: to abort: touch $TD_STOP"

    if [ "$bin" != "$TD_DIR/bin/tetrad" ]; then
        cp "$TD_DIR/bin/tetrad" "$bin.part" && chmod 755 "$bin.part" || return 1
        if [ "$(md5sum <"$TD_DIR/bin/tetrad")" != "$(md5sum <"$bin.part")" ]; then
            echo "tetrad-stage: the copy in /tmp differs from $TD_DIR/bin/tetrad; stock ZenUI still runs" >&2
            rm -f "$bin.part"
            return 1
        fi
        mv "$bin.part" "$bin" || return 1
    fi
    # A typo costs no reboot: stock still runs when the check fails (review B11).
    if ! "$bin" bringup --check --stage "$stage" --time-limit "$seconds" \
        --log-dir "$log" "$@" </dev/null >/dev/null; then
        echo "tetrad-stage: tetrad refused the options; stock ZenUI still runs" >&2
        return 2
    fi
    mkdir -p "$log" || return 1
    echo "stage $stage limit $seconds args $*" > "$log/stage.txt"
    # Reference time for `find /media/p4 /opt/neuraldsp -newer` after the run (S1).
    touch "$log/start.stamp"
    rm -f "$TD_STOP"
    if [ -n "$stock" ]; then
        # $stock is a list of pids.
        # shellcheck disable=SC2086
        kill $stock
        i=0
        while [ $i -lt $TD_STOCK_WAIT ] && pidof ZenUI >/dev/null; do
            usleep 100000
            i=$((i + 1))
        done
        stock=$(td_stock_pids)
        if [ -n "$stock" ]; then
            echo "tetrad-stage: stock ZenUI did not quit in 5 s: SIGKILL" | tee -a "$log/stage.txt"
            # shellcheck disable=SC2086
            kill -9 $stock
        fi
    fi
    "$bin" bringup --stage "$stage" --time-limit "$seconds" --log-dir "$log" "$@" \
        </dev/null >"$log/stderr.log" 2>&1 &
    echo "tetrad-stage: tetrad started, pid $!"
    # After the start: stock (and its process killer) is gone. Compare with the upload md5.
    md5sum "$bin" | tee -a "$log/stage.txt"
}

case ${0##*/} in
    tetrad-stage.sh) td_stage "$@"; exit $? ;;
esac
