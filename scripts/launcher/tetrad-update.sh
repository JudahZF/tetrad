#!/bin/sh
# tetrad-update.sh: patched CorOS install that keeps the Tetrad boot hook and the user's SSH keys.
# Design: docs/updates.md. Tested in a QEMU VM with a synthetic package and the real CorOS 4.1.1
# package, then on one unit (CorOS 4.1.0 to 4.1.1, started from the Tetrad INSTALL button).
# BusyBox ash 1.23.1, run as root:   sh tetrad-update.sh
#
# It runs the RUNNING root's stock /opt/neuraldsp/sysupdate_run.sh with one added line just
# before STEP_13 (the p3 one/two flip, the commit point). The stock file is not changed: a copy
# with the added line is made in /tmp, and only when the stock file has the pinned md5 and the
# STEP_13 line occurs exactly once. At the added line the stock script has already written the
# new root and still has it mounted read-write at /mnt/new_zenos_mount. The added step then:
#   1. re-applies the hook: `tetrad-launcher.sh install-hook` with TD_ROOT = the new root (the same
#      md5-pinned S80drvinit edit as on the running root), and reads the state back (md5),
#   2. appends to the new root's /root/.ssh/authorized_keys each key line of the running root's
#      file that it does not have (keys only, no comments; 0600, root:root), and reads it back,
#   3. syncs, unmounts the new root and checks it with `e2fsck -fn`.
# Only then does the stock STEP_13 flip the marker. A failure in the step cleans up like the
# stock failure branches and exits 218 before the flip: p3 does not change, the old root boots.
#
# Exit codes: the stock codes (0, 1, 201-217); 218 = the added step failed (marker unchanged);
# 220 = refused before the stock script starts (nothing written).
# Sourcing the file defines the functions and runs nothing (the host test sources it).

TD_UPD_STOCK=${TD_UPD_STOCK:-/opt/neuraldsp/sysupdate_run.sh}
TD_UPD_STOCK_MD5=6158487f540bf6c5af652e38fb009101 # sysupdate_run.sh of CorOS 4.0.1, 4.1.0, 4.1.1
# Lines of the stock script: its own variables, expanded when it runs.
# shellcheck disable=SC2016
TD_UPD_CUT='echo "UPDATER_STEP_13" > ${ui_status_file}'
# shellcheck disable=SC2016
TD_UPD_STEP='td_upd_step "${new_zenos_partition_mount}" "${new_root}" || td_upd_fail'
TD_UPD_RUN=${TD_UPD_RUN:-/tmp/tetrad-sysupdate.sh}
TD_UPD_PACKAGE=${TD_UPD_PACKAGE:-/media/p4/UpdaterTemp/update.bin}
TD_UPD_LAUNCHER=${TD_UPD_LAUNCHER:-/media/p4/tetrad/launcher/tetrad-launcher.sh}
TD_UPD_KEYS=${TD_UPD_KEYS:-/root/.ssh/authorized_keys}

td_upd_log() {
    echo "tetrad-update: $*"
}

# $1 new root: the hook, through the launcher's own install-hook and its md5 pins.
td_upd_hook() {
    (
        # shellcheck disable=SC2034 # read by the sourced launcher
        TD_ROOT=$1
        # shellcheck source-path=SCRIPTDIR source=tetrad-launcher.sh
        . "$TD_UPD_LAUNCHER"
        td_install_hook || exit 1
        [ "$(td_hook_state)" = hooked ] || {
            td_upd_log "S80drvinit read-back: md5 $(td_md5 "$TD_INIT") is not the hooked file" >&2
            exit 1
        }
        td_upd_log "hook ok: $TD_INIT md5 $(td_md5 "$TD_INIT")"
    )
}

# $1 new root: the running root's key lines that the new root's authorized_keys lacks.
td_upd_keys() {
    td_dir=$1/root/.ssh
    td_new=$td_dir/authorized_keys
    td_tmp=$td_dir/.authorized_keys.tetrad-new
    [ -f "$TD_UPD_KEYS" ] || { td_upd_log "no $TD_UPD_KEYS: no keys to keep"; return 0; }
    if [ ! -d "$td_dir" ]; then
        mkdir -p "$td_dir" && chmod 700 "$td_dir" || return 1
    fi
    rm -f "$td_tmp"
    if [ -f "$td_new" ]; then
        cat "$td_new" >"$td_tmp" || return 1
        # A last line without a newline would join the first added key.
        [ -s "$td_tmp" ] && [ "$(tail -c 1 "$td_tmp" | wc -l)" -eq 0 ] && echo >>"$td_tmp"
    fi
    td_added=0
    while IFS= read -r td_line || [ -n "$td_line" ]; do
        case $td_line in '' | '#'*) continue ;; esac
        [ -f "$td_tmp" ] && grep -qxF -e "$td_line" "$td_tmp" && continue
        echo "$td_line" >>"$td_tmp" || return 1
        td_added=$((td_added + 1))
    done <"$TD_UPD_KEYS"
    if [ "$td_added" -gt 0 ] || [ ! -f "$td_new" ]; then
        chmod 600 "$td_tmp" && sync && mv -f "$td_tmp" "$td_new" && sync || return 1
    else
        rm -f "$td_tmp"
        chmod 600 "$td_new" || return 1
    fi
    # Read back: every key line is there, and sshd's StrictModes accepts the file and the dir.
    while IFS= read -r td_line || [ -n "$td_line" ]; do
        case $td_line in '' | '#'*) continue ;; esac
        grep -qxF -e "$td_line" "$td_new" || { td_upd_log "key read-back failed for $td_new" >&2; return 1; }
    done <"$TD_UPD_KEYS"
    [ "$(stat -c '%a %u %g' "$td_new")" = "600 0 0" ] ||
        { td_upd_log "$td_new is $(stat -c '%a %u %g' "$td_new"), not 600 0 0" >&2; return 1; }
    [ "$(stat -c '%a %u %g' "$td_dir")" = "700 0 0" ] ||
        { td_upd_log "$td_dir is $(stat -c '%a %u %g' "$td_dir"), not 700 0 0" >&2; return 1; }
    td_upd_log "keys ok: $td_added added, $(grep -c . "$td_new") line(s) in $td_new"
}

# The added step. $1 new root mount, $2 new root device. Runs inside the stock script's shell.
td_upd_step() {
    td_upd_log "re-apply on $2 ($1), before the commit point"
    td_upd_hook "$1" || return 1
    td_upd_keys "$1" || return 1
    sync
    umount "$1" || { td_upd_log "cannot unmount $1" >&2; return 1; }
    e2fsck -fn "$2" >/dev/null 2>&1 || { td_upd_log "e2fsck -fn $2 found errors" >&2; return 1; }
    td_upd_log "re-apply ok; $2 unmounted and clean; the commit follows"
}

# Failure of the added step: the cleanup of the stock STEP_12 failure branch (it uses the stock
# script's variables), then exit 218. Unlike stock, it unmounts the rootfs image and removes the
# mount points with rmdir, so no `rm -rf` runs into a mounted file system. The commands keep the
# stock form (unquoted stock variables), so the cleanup is the same as stock's.
# shellcheck disable=SC2086,SC2115,SC2154
td_upd_fail() {
    echo "fail"
    td_upd_log "the added step failed; ${flag_partition} is not changed, ${current_root} stays the boot root"
    rm -rf $updater_new_version_downloaded
    sync
    rm -rf ${UPDATE_PACKAGE_TMP_COMPRESSED_FILE}
    rm -rf ${UPDATE_PACKAGE_TMP_UNCOMPRESSED_FILE}
    rm -rf ${UPDATE_PACKAGE_FILE}
    sync
    if mount | grep -q " ${new_zenos_partition_mount} "; then
        rm -rf ${new_zenos_partition_mount}/*
        umount ${new_zenos_partition_mount}
    fi
    umount ${rootfs_file_mount}
    rm -rf ${update_file_mount}/*
    umount ${flag_partition_mount}
    rmdir ${rootfs_file_mount} ${update_file_mount} ${flag_partition_mount} ${new_zenos_partition_mount}
    echo "UPDATER_FAILURE" > ${ui_status_file}
    sync
    exit 218
}

# $1 this file (absolute). Writes $TD_UPD_RUN: a header that sources this file, then the stock
# script with $TD_UPD_STEP before the STEP_13 line.
td_upd_make() {
    {
        echo '#!/bin/sh'
        echo "# Made by $1 from $TD_UPD_STOCK: one line added before STEP_13."
        printf ". '%s'\n" "$1"
        TD_A=$TD_UPD_CUT TD_B=$TD_UPD_STEP awk '
            $0 == ENVIRON["TD_A"] { print ENVIRON["TD_B"] }
            { print }' "$TD_UPD_STOCK"
    } >"$TD_UPD_RUN" || return 1
    sh -n "$TD_UPD_RUN" && [ "$(grep -cxF -e "$TD_UPD_STEP" "$TD_UPD_RUN")" = 1 ]
}

td_upd_main() {
    refuse() { td_upd_log "$* ; refused, nothing written" >&2; exit 220; }
    [ "$(id -u)" = 0 ] || refuse "not root"
    have=$(md5sum <"$TD_UPD_STOCK" | cut -c1-32)
    [ "$have" = "$TD_UPD_STOCK_MD5" ] || refuse "$TD_UPD_STOCK md5 $have is not the pinned installer"
    [ "$(grep -cxF -e "$TD_UPD_CUT" "$TD_UPD_STOCK")" = 1 ] || refuse "no single STEP_13 line"
    sh -n "$TD_UPD_LAUNCHER" 2>/dev/null || refuse "$TD_UPD_LAUNCHER is missing or does not parse"
    [ -f "$TD_UPD_PACKAGE" ] || refuse "no $TD_UPD_PACKAGE"
    td_upd_log "gzip -t $TD_UPD_PACKAGE"
    gzip -t "$TD_UPD_PACKAGE" || refuse "$TD_UPD_PACKAGE fails gzip -t"
    case $0 in /*) self=$0 ;; *) self=$(pwd)/$0 ;; esac
    td_upd_make "$self" || { rm -f "$TD_UPD_RUN"; refuse "cannot make $TD_UPD_RUN"; }
    td_upd_log "running $TD_UPD_RUN (stock md5 $have + the re-apply step)"
    cd /opt/neuraldsp && exec sh "$TD_UPD_RUN"
}

case ${0##*/} in
    tetrad-update.sh) td_upd_main "$@"; exit $? ;;
esac
