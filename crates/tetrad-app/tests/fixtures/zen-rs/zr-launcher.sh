#!/bin/sh
# shellcheck disable=SC2016 # the `$` words of the hook lines are S80drvinit text
# Test fixture (tests/install_script.rs): the launcher tool of zen-rs, Tetrad's old working
# name, as it is on a zen-rs unit. Only its comments are shortened.
# zr-launcher.sh COMMAND: operator side of the zen-rs boot launcher on the QC (BusyBox ash
# 1.23, run as root over SSH). Design, states and recovery: docs/launcher.md.
#
#   status        hook state, selection, disabled reason, unconfirmed starts, last log lines
#   arm           select the zenui-rs binary now in bin/ (its md5), clear disabled and pending
#   disarm        remove the selection: stock ZenUI from the next boot on (the hook stays)
#   chooser on|off  the boot selector screen of an armed boot (off: zen-rs starts, no screen)
#   install-hook  the one rootfs change: S80drvinit runs zr-launch.sh in place of ./ZenUI
#   remove-hook   S80drvinit back to the CorOS 4.1.0 file, byte for byte
#
# The hook commands change /etc/init.d/S80drvinit only when its md5 is a known one, write the
# new file beside it in /etc (rcS runs every /etc/init.d/S??*), check the result's md5 and
# then rename it into place. Nothing here writes /opt/neuraldsp or a partition.
# Sourcing the file defines the functions and runs nothing (host tests set ZR_ROOT).

ZR_ROOT=${ZR_ROOT:-}
ZR_HOME=$ZR_ROOT/media/p4/zenui-rs/launcher
ZR_BIN=$ZR_ROOT/media/p4/zenui-rs/bin/zenui-rs
ZR_INIT=$ZR_ROOT/etc/init.d/S80drvinit
ZR_INIT_NEW=$ZR_ROOT/etc/.S80drvinit.zr-new
# S80drvinit of CorOS 4.1.0 and the same file with the hook (zr_hook_patch).
ZR_INIT_STOCK_MD5=688ddb2805f34c69a8a1d7d173a406b7
ZR_INIT_HOOKED_MD5=d6bafded21b2192dc18927f575fbb742

# S80drvinit:351, the stock UI start, and the hooked form. The hook line picks the launcher
# only when the file exists and parses (`sh -n`); in every other case ./ZenUI starts as before.
ZR_LINE_TAIL='QC_SERIAL_NUMBER=/media/p4/qc_sn ./ZenUI 2>&1 | tee -a "$SYSTEM_LOG_FILE" &'
ZR_LINE_HEAD='QWS_MOUSE_PROTO=tslib TSLIB_FBDEVICE=/dev/fb0 TSLIB_CONSOLEDEVICE=none TSLIB_CONFFILE=/etc/ts.conf TSLIB_CALIBFILE=/etc/pointercal TSLIB_TSDEVICE=/dev/input/event0'
ZR_LINE_STOCK=$(printf '\t\t%s %s' "$ZR_LINE_HEAD" "$ZR_LINE_TAIL")
ZR_LINE_HOOKED=$(printf '\t\t%s %s' "$ZR_LINE_HEAD" \
    'QC_SERIAL_NUMBER=/media/p4/qc_sn $ZR_UI 2>&1 | tee -a "$SYSTEM_LOG_FILE" &')
ZR_LINE_HOOK=$(printf '\t\t%s' \
    'ZR_UI=./ZenUI; ZR_LAUNCH=/media/p4/zenui-rs/launcher/zr-launch.sh; [ -f $ZR_LAUNCH ] && sh -n $ZR_LAUNCH 2>/dev/null && ZR_UI="sh $ZR_LAUNCH" # zen-rs launcher hook')

zr_md5() {
    md5sum <"$1" | cut -c1-32
}

# stdin → stdout: the stock start line becomes the hook line and the hooked start line.
zr_hook_patch() {
    ZR_A=$ZR_LINE_STOCK ZR_B=$ZR_LINE_HOOK ZR_C=$ZR_LINE_HOOKED awk '
        $0 == ENVIRON["ZR_A"] { print ENVIRON["ZR_B"]; print ENVIRON["ZR_C"]; next }
        { print }'
}

# stdin → stdout: the inverse of zr_hook_patch.
zr_hook_unpatch() {
    ZR_A=$ZR_LINE_STOCK ZR_B=$ZR_LINE_HOOK ZR_C=$ZR_LINE_HOOKED awk '
        $0 == ENVIRON["ZR_B"] { next }
        $0 == ENVIRON["ZR_C"] { print ENVIRON["ZR_A"]; next }
        { print }'
}

# $1 filter, $2 md5 S80drvinit must have now, $3 md5 it must have after.
zr_hook_rewrite() {
    "$1" <"$ZR_INIT" >"$ZR_INIT_NEW" || { rm -f "$ZR_INIT_NEW"; return 1; }
    if [ "$(zr_md5 "$ZR_INIT_NEW")" != "$3" ]; then
        echo "zr-launcher: the rewritten S80drvinit has md5 $(zr_md5 "$ZR_INIT_NEW"), not $3; nothing changed" >&2
        rm -f "$ZR_INIT_NEW"
        return 1
    fi
    chmod 755 "$ZR_INIT_NEW" && sync && mv -f "$ZR_INIT_NEW" "$ZR_INIT" && sync || return 1
    echo "zr-launcher: S80drvinit md5 $2 -> $(zr_md5 "$ZR_INIT")"
}

zr_hook_state() {
    case $(zr_md5 "$ZR_INIT") in
        "$ZR_INIT_STOCK_MD5") echo stock ;;
        "$ZR_INIT_HOOKED_MD5") echo hooked ;;
        *) echo unknown ;;
    esac
}

zr_install_hook() {
    case $(zr_hook_state) in
        hooked) echo "zr-launcher: the hook is already installed"; return 0 ;;
        unknown)
            echo "zr-launcher: S80drvinit md5 $(zr_md5 "$ZR_INIT") is not the CorOS 4.1.0 file; refused" >&2
            return 1
            ;;
    esac
    zr_hook_rewrite zr_hook_patch "$ZR_INIT_STOCK_MD5" "$ZR_INIT_HOOKED_MD5"
}

zr_remove_hook() {
    case $(zr_hook_state) in
        stock) echo "zr-launcher: the hook is not installed"; return 0 ;;
        unknown)
            echo "zr-launcher: S80drvinit md5 $(zr_md5 "$ZR_INIT") is not a known file; refused" >&2
            return 1
            ;;
    esac
    zr_hook_rewrite zr_hook_unpatch "$ZR_INIT_HOOKED_MD5" "$ZR_INIT_STOCK_MD5"
}

zr_arm() {
    [ -x "$ZR_BIN" ] || { echo "zr-launcher: $ZR_BIN is missing or not executable" >&2; return 1; }
    sh -n "$ZR_HOME/zr-launch.sh" || {
        echo "zr-launcher: $ZR_HOME/zr-launch.sh is missing or does not parse" >&2
        return 1
    }
    zr_md5 "$ZR_BIN" >"$ZR_HOME/select.tmp" && mv -f "$ZR_HOME/select.tmp" "$ZR_HOME/select" &&
        rm -f "$ZR_HOME/disabled" "$ZR_HOME/pending" && sync || return 1
    echo "zr-launcher: armed zenui-rs md5 $(cat "$ZR_HOME/select") (hook: $(zr_hook_state))"
}

zr_disarm() {
    rm -f "$ZR_HOME/select" && sync || return 1
    echo "zr-launcher: disarmed; stock ZenUI runs from the next boot on"
}

zr_chooser() {
    case ${1:-} in
        on) rm -f "$ZR_HOME/no-chooser" ;;
        off) echo "turned off by zr-launcher.sh" >"$ZR_HOME/no-chooser" ;;
        *) echo "zr-launcher: chooser on|off" >&2; return 2 ;;
    esac
    sync && echo "zr-launcher: boot selector $1"
}

zr_status() {
    echo "hook:     $(zr_hook_state) (S80drvinit md5 $(zr_md5 "$ZR_INIT"))"
    if [ -f "$ZR_HOME/select" ]; then
        echo "select:   $(cat "$ZR_HOME/select") (bin/zenui-rs md5 $(zr_md5 "$ZR_BIN" 2>/dev/null))"
    else
        echo "select:   none (stock ZenUI)"
    fi
    if [ -f "$ZR_HOME/no-chooser" ]; then
        echo "chooser:  off"
    else
        echo "chooser:  on (saved choice: $(cat "$ZR_HOME/boot-choice" 2>/dev/null || echo zen-rs))"
    fi
    [ -f "$ZR_HOME/disabled" ] && echo "disabled: $(cat "$ZR_HOME/disabled")"
    [ -f "$ZR_HOME/pending" ] && echo "pending:  $(cat "$ZR_HOME/pending") unconfirmed start(s)"
    [ -f "$ZR_HOME/launch.log" ] && tail -n 5 "$ZR_HOME/launch.log"
    return 0
}

zr_launcher() {
    case ${1:-} in
        status) zr_status ;;
        arm) zr_arm ;;
        disarm) zr_disarm ;;
        chooser) zr_chooser "${2:-}" ;;
        install-hook) zr_install_hook ;;
        remove-hook) zr_remove_hook ;;
        *) sed -n '2,11p' "$0"; return 2 ;;
    esac
}

case ${0##*/} in
    zr-launcher.sh) zr_launcher "$@"; exit $? ;;
esac
