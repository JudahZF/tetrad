#!/bin/sh
# tetrad-launcher.sh COMMAND: operator side of the Tetrad boot launcher on the QC (BusyBox ash
# 1.23, run as root over SSH). Design, states and recovery: docs/launcher.md.
#
#   status        hook state, selection, disabled reason, unconfirmed starts, last log lines
#   arm           select the tetrad binary now in bin/ (its md5), clear disabled and pending
#   disarm        remove the selection: stock ZenUI from the next boot on (the hook stays)
#   chooser on|off  the boot selector screen of an armed boot (off: Tetrad starts, no screen)
#   install-hook  the one rootfs change: S80drvinit runs tetrad-launch.sh in place of ./ZenUI
#   remove-hook   S80drvinit back to the CorOS 4.1.0 file, byte for byte
#
# The hook commands change /etc/init.d/S80drvinit only when its md5 is a known one, write the
# new file beside it in /etc (rcS runs every /etc/init.d/S??*), check the result's md5 and
# then rename it into place. Nothing here writes /opt/neuraldsp or a partition.
# Sourcing the file defines the functions and runs nothing (host tests set TD_ROOT).

TD_ROOT=${TD_ROOT:-}
TD_HOME=$TD_ROOT/media/p4/tetrad/launcher
TD_BIN=$TD_ROOT/media/p4/tetrad/bin/tetrad
TD_INIT=$TD_ROOT/etc/init.d/S80drvinit
TD_INIT_NEW=$TD_ROOT/etc/.S80drvinit.tetrad-new
# S80drvinit of CorOS 4.1.0 (4.1.1 has the same file) and the same file with the hook
# (td_hook_patch). A host test checks both when QC_CORPUS_DIR has a copy of the file.
TD_INIT_STOCK_MD5=688ddb2805f34c69a8a1d7d173a406b7
TD_INIT_HOOKED_MD5=1a259e0f0031a179df6e8a0854731421

# S80drvinit:351, the stock UI start, and the hooked form. The hook line picks the launcher
# only when the file exists and parses (`sh -n`); in every other case ./ZenUI starts as before.
# The `$` words in these lines are S80drvinit text; init expands them, not this script.
# shellcheck disable=SC2016
TD_LINE_TAIL='QC_SERIAL_NUMBER=/media/p4/qc_sn ./ZenUI 2>&1 | tee -a "$SYSTEM_LOG_FILE" &'
TD_LINE_HEAD='QWS_MOUSE_PROTO=tslib TSLIB_FBDEVICE=/dev/fb0 TSLIB_CONSOLEDEVICE=none TSLIB_CONFFILE=/etc/ts.conf TSLIB_CALIBFILE=/etc/pointercal TSLIB_TSDEVICE=/dev/input/event0'
TD_LINE_STOCK=$(printf '\t\t%s %s' "$TD_LINE_HEAD" "$TD_LINE_TAIL")
# shellcheck disable=SC2016
TD_LINE_HOOKED=$(printf '\t\t%s %s' "$TD_LINE_HEAD" \
    'QC_SERIAL_NUMBER=/media/p4/qc_sn $TD_UI 2>&1 | tee -a "$SYSTEM_LOG_FILE" &')
# shellcheck disable=SC2016
TD_LINE_HOOK=$(printf '\t\t%s' \
    'TD_UI=./ZenUI; TD_LAUNCH=/media/p4/tetrad/launcher/tetrad-launch.sh; [ -f $TD_LAUNCH ] && sh -n $TD_LAUNCH 2>/dev/null && TD_UI="sh $TD_LAUNCH" # Tetrad launcher hook')

td_md5() {
    md5sum <"$1" | cut -c1-32
}

# stdin → stdout: the stock start line becomes the hook line and the hooked start line.
td_hook_patch() {
    TD_A=$TD_LINE_STOCK TD_B=$TD_LINE_HOOK TD_C=$TD_LINE_HOOKED awk '
        $0 == ENVIRON["TD_A"] { print ENVIRON["TD_B"]; print ENVIRON["TD_C"]; next }
        { print }'
}

# stdin → stdout: the inverse of td_hook_patch.
td_hook_unpatch() {
    TD_A=$TD_LINE_STOCK TD_B=$TD_LINE_HOOK TD_C=$TD_LINE_HOOKED awk '
        $0 == ENVIRON["TD_B"] { next }
        $0 == ENVIRON["TD_C"] { print ENVIRON["TD_A"]; next }
        { print }'
}

# $1 filter, $2 md5 S80drvinit must have now, $3 md5 it must have after.
td_hook_rewrite() {
    "$1" <"$TD_INIT" >"$TD_INIT_NEW" || { rm -f "$TD_INIT_NEW"; return 1; }
    if [ "$(td_md5 "$TD_INIT_NEW")" != "$3" ]; then
        echo "tetrad-launcher: the rewritten S80drvinit has md5 $(td_md5 "$TD_INIT_NEW"), not $3; nothing changed" >&2
        rm -f "$TD_INIT_NEW"
        return 1
    fi
    chmod 755 "$TD_INIT_NEW" && sync && mv -f "$TD_INIT_NEW" "$TD_INIT" && sync || return 1
    echo "tetrad-launcher: S80drvinit md5 $2 -> $(td_md5 "$TD_INIT")"
}

td_hook_state() {
    case $(td_md5 "$TD_INIT") in
        "$TD_INIT_STOCK_MD5") echo stock ;;
        "$TD_INIT_HOOKED_MD5") echo hooked ;;
        *) echo unknown ;;
    esac
}

td_install_hook() {
    case $(td_hook_state) in
        hooked) echo "tetrad-launcher: the hook is already installed"; return 0 ;;
        unknown)
            echo "tetrad-launcher: S80drvinit md5 $(td_md5 "$TD_INIT") is not the CorOS 4.1.0 file; refused" >&2
            return 1
            ;;
    esac
    td_hook_rewrite td_hook_patch "$TD_INIT_STOCK_MD5" "$TD_INIT_HOOKED_MD5"
}

td_remove_hook() {
    case $(td_hook_state) in
        stock) echo "tetrad-launcher: the hook is not installed"; return 0 ;;
        unknown)
            echo "tetrad-launcher: S80drvinit md5 $(td_md5 "$TD_INIT") is not a known file; refused" >&2
            return 1
            ;;
    esac
    td_hook_rewrite td_hook_unpatch "$TD_INIT_HOOKED_MD5" "$TD_INIT_STOCK_MD5"
}

td_arm() {
    [ -x "$TD_BIN" ] || { echo "tetrad-launcher: $TD_BIN is missing or not executable" >&2; return 1; }
    sh -n "$TD_HOME/tetrad-launch.sh" || {
        echo "tetrad-launcher: $TD_HOME/tetrad-launch.sh is missing or does not parse" >&2
        return 1
    }
    td_md5 "$TD_BIN" >"$TD_HOME/select.tmp" && mv -f "$TD_HOME/select.tmp" "$TD_HOME/select" &&
        rm -f "$TD_HOME/disabled" "$TD_HOME/pending" && sync || return 1
    echo "tetrad-launcher: armed tetrad md5 $(cat "$TD_HOME/select") (hook: $(td_hook_state))"
}

td_disarm() {
    rm -f "$TD_HOME/select" && sync || return 1
    echo "tetrad-launcher: disarmed; stock ZenUI runs from the next boot on"
}

td_chooser() {
    case ${1:-} in
        on) rm -f "$TD_HOME/no-chooser" ;;
        off) echo "turned off by tetrad-launcher.sh" >"$TD_HOME/no-chooser" ;;
        *) echo "tetrad-launcher: chooser on|off" >&2; return 2 ;;
    esac
    sync && echo "tetrad-launcher: boot selector $1"
}

td_status() {
    echo "hook:     $(td_hook_state) (S80drvinit md5 $(td_md5 "$TD_INIT"))"
    if [ -f "$TD_HOME/select" ]; then
        echo "select:   $(cat "$TD_HOME/select") (bin/tetrad md5 $(td_md5 "$TD_BIN" 2>/dev/null))"
    else
        echo "select:   none (stock ZenUI)"
    fi
    if [ -f "$TD_HOME/no-chooser" ]; then
        echo "chooser:  off"
    else
        echo "chooser:  on (saved choice: $(cat "$TD_HOME/boot-choice" 2>/dev/null || echo tetrad))"
    fi
    [ -f "$TD_HOME/disabled" ] && echo "disabled: $(cat "$TD_HOME/disabled")"
    [ -f "$TD_HOME/pending" ] && echo "pending:  $(cat "$TD_HOME/pending") unconfirmed start(s)"
    [ -f "$TD_HOME/launch.log" ] && tail -n 5 "$TD_HOME/launch.log"
    return 0
}

td_launcher() {
    case ${1:-} in
        status) td_status ;;
        arm) td_arm ;;
        disarm) td_disarm ;;
        chooser) td_chooser "${2:-}" ;;
        install-hook) td_install_hook ;;
        remove-hook) td_remove_hook ;;
        *) sed -n '2,11p' "$0"; return 2 ;;
    esac
}

case ${0##*/} in
    tetrad-launcher.sh) td_launcher "$@"; exit $? ;;
esac
