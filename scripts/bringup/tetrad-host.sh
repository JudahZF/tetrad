#!/usr/bin/env bash
# tetrad-host.sh COMMAND [ARGS]: host side of the tetrad bring-up (developer tool).
#
# Nothing runs unless you run a command. Every command prints what it will do first.
# Commands that change the QC (upload, presets, stage) ask you to type "yes".
# Sourcing this file runs nothing. All commands share one SSH connection (ControlMaster,
# kept 2 h), so `stop` reaches the QC without a new proxy handshake.
#
#   upload BIN        maintenance-window upload of tetrad and tetrad-stage.sh, md5 check, R2
#   presets NAME...   copy stock-saved test presets from setlist TD_SETLIST to the tetrad folder
#   stage S SECS ...  run one stage: sh tetrad-stage.sh S SECS [tetrad bringup args]
#   stop              touch the stop file (abort; no prompt)
#   logs              list the run folders on the QC
#   pull RUN [DEST]   copy one run folder, one `cat` per file (default .local/tetrad-logs/RUN)
#   launcher          upload scripts/launcher/*.sh to TD_DIR/launcher (no arm, no init change)
#
# Environment: QC_HOST (ssh host, default qc), TD_DIR (default /media/p4/tetrad), TD_SETLIST
# (the stock setlist that `presets` copies from, default "Tetrad Test"; it is the folder
# /media/p4/Presets/TD_SETLIST on the QC).

[[ "${BASH_SOURCE[0]}" == "$0" ]] || return 0

set -euo pipefail

HOST=${QC_HOST:-qc}
D=${TD_DIR:-/media/p4/tetrad}
SETLIST=${TD_SETLIST:-Tetrad Test}
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
# One connection per host for every command (review B15). A dead master (the QC rebooted)
# ends within 15 s; the next command opens a new one.
SSH_OPTS=(
    -o ControlMaster=auto
    -o "ControlPath=${XDG_RUNTIME_DIR:-/tmp}/tetrad-ssh-%C"
    -o ControlPersist=2h
    -o ServerAliveInterval=5
    -o ServerAliveCountMax=3
)
# Stock ZenUI stops as its own SIGTERM handler allows (unixSignal -> QApplication::quit, then
# CZenjack::Close): a write in progress finishes. SIGKILL only after 5 s (review B12).
# shellcheck disable=SC2016 # runs on the QC
STOP_STOCK='p=$(pidof ZenUI); if [ -n "$p" ]; then kill $p; i=0; while [ $i -lt 50 ] && pidof ZenUI >/dev/null; do usleep 100000; i=$((i + 1)); done; p=$(pidof ZenUI); [ -z "$p" ] || kill -9 $p; fi'

die() {
    echo "tetrad-host: $*" >&2
    exit 1
}

confirm() {
    local answer
    read -r -p "Type yes to run these steps on $HOST: " answer
    [[ $answer == yes ]] || die "not confirmed; nothing was run"
}

# One plain name: no slash, quote or space reaches a remote shell line.
plain() {
    [[ $1 =~ ^[A-Za-z0-9._-]+$ ]] || die "'$1' is not a plain file name"
}

# One command on the QC over the shared connection.
qc() {
    ssh -T "${SSH_OPTS[@]}" "$HOST" "$@"
}

# R2: back to stock. Runs from the EXIT trap of `upload`, so a failed step cannot skip it.
r2() {
    echo "tetrad-host: R2 reboot to stock"
    qc /opt/neuraldsp/zenreboot ||
        echo "tetrad-host: R2 failed: run 'ssh $HOST /opt/neuraldsp/zenreboot' or power-cycle (R3)" >&2
    ssh "${SSH_OPTS[@]}" -O exit "$HOST" 2>/dev/null || true
}

upload() {
    local bin=${1:?usage: upload path/to/tetrad}
    local script=$HERE/tetrad-stage.sh
    [[ -f $bin ]] || die "no binary at $bin"
    local bin_md5 script_md5
    bin_md5=$(md5sum "$bin" | cut -d' ' -f1)
    script_md5=$(md5sum "$script" | cut -d' ' -f1)
    cat <<EOF
Maintenance-window upload to $HOST:$D/bin (plan §1.5):
  1. stop ZenWatchdog (SIGTERM), then stock ZenUI: SIGTERM, wait up to 5 s, then SIGKILL
     (the process killer stops with it)
  2. mkdir -p $D/bin $D/logs $D/presets
  3. per file: cat > ….part, then md5sum of the .part; only a matching .part is
     chmod 755 and moved into place (a partial or wrong file never is)
  4. md5sum of both files in place: tetrad $bin_md5, tetrad-stage.sh $script_md5
  5. /opt/neuraldsp/zenreboot (R2: back to stock). From step 1 on it runs on every exit,
     also after a failed step.
EOF
    confirm
    trap r2 EXIT
    qc "kill \$(pidof ZenWatchdog); $STOP_STOCK" || true
    copy_and_check "$bin" "$script" "$bin_md5" "$script_md5"
}

# Every step is checked with `|| return 1`: the caller may run this in a context where
# `set -e` does not apply (review B3).
copy_and_check() {
    local -a froms=("$1" "$2") names=(tetrad tetrad-stage.sh) sums=("$3" "$4")
    qc "mkdir -p $D/bin $D/logs $D/presets" || return 1
    local i target sum
    for i in 0 1; do
        target=$D/bin/${names[i]}
        echo "tetrad-host: copy ${froms[i]} -> $target.part"
        qc "cat > $target.part" <"${froms[i]}" || return 1
        sum=$(qc "md5sum $target.part") || return 1
        if [[ ${sum%% *} != "${sums[i]}" ]]; then
            echo "tetrad-host: $target.part md5 ${sum%% *} is not ${sums[i]}; not moved" >&2
            qc "rm -f $target.part" || true
            return 1
        fi
        qc "chmod 755 $target.part && mv $target.part $target" || return 1
    done
    local remote
    remote=$(qc "md5sum $D/bin/tetrad $D/bin/tetrad-stage.sh") || return 1
    echo "$remote"
    for i in 0 1; do
        if ! grep -q "^${sums[i]} " <<<"$remote"; then
            echo "tetrad-host: ${names[i]} md5 differs on the QC" >&2
            return 1
        fi
    done
    echo "tetrad-host: md5 match"
}

presets() {
    (($# > 0)) || die "usage: presets T1.pb [T2.pb ...]"
    # Spaces are allowed: the name is single-quoted on the remote shell line.
    [[ $SETLIST =~ ^[A-Za-z0-9_-][A-Za-z0-9._\ -]*$ ]] ||
        die "TD_SETLIST '$SETLIST' is not a plain setlist name"
    local name
    for name; do plain "$name"; done
    echo "Will copy on $HOST, for each name: cp '/media/p4/Presets/$SETLIST/NAME' $D/presets/"
    echo "  names: $*"
    confirm
    for name; do
        qc "cp '/media/p4/Presets/$SETLIST/$name' $D/presets/"
    done
}

stage() {
    (($# >= 2)) || die "usage: stage probe|dsp|deploy|full SECONDS [tetrad bringup args]"
    local args
    printf -v args ' %q' "$@"
    local line="sh $D/bin/tetrad-stage.sh$args"
    cat <<EOF
Will run on $HOST: $line
It checks the options, stops stock ZenUI and starts tetrad for $2 s; ZenWatchdog restores
stock after it (R1).
Abort: pull the output cable if audio is wrong, then: $0 stop
EOF
    confirm
    qc "$line" </dev/null
}

stop() {
    echo "Will run on $HOST: touch /tmp/tetrad.stop"
    qc 'touch /tmp/tetrad.stop'
}

logs() {
    echo "Will run on $HOST: ls -1 $D/logs"
    qc "ls -1 $D/logs"
}

# Writes under .local (not tracked) unless DEST is given. Every device
# file name is checked before the first copy: plain path parts only, no `.` or `..`.
pull() {
    local run=${1:?usage: pull RUN [DEST]}
    plain "$run"
    local dest=${2:-$REPO/.local/tetrad-logs/$run}
    echo "Will list $HOST:$D/logs/$run and copy each file with one 'cat' to $dest"
    local files file rel
    files=$(qc "find $D/logs/$run -type f" </dev/null)
    while IFS= read -r file; do
        [[ -n $file ]] || continue
        rel=${file#"$D/logs/$run/"}
        if [[ $file != "$D/logs/$run/$rel" || ! $rel =~ ^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*$ ||
            /$rel/ == */../* || /$rel/ == */./* ]]; then
            die "refused: unexpected device file name '$file'; nothing was copied"
        fi
    done <<<"$files"
    while IFS= read -r file; do
        [[ -n $file ]] || continue
        rel=${file#"$D/logs/$run/"}
        mkdir -p "$dest/$(dirname "$rel")"
        # </dev/null: ssh would read the rest of the file list from this loop's stdin.
        qc "cat '$file'" </dev/null >"$dest/$rel"
        echo "  $rel ($(wc -c <"$dest/$rel") bytes)"
    done <<<"$files"
}

# The boot launcher scripts (docs/launcher.md), each through .part and an md5 check. Stock
# keeps running: the files are small. Arming and the init hook stay manual steps on the QC
# (`tetrad-launcher.sh install-hook`, `arm`).
launcher() {
    local name sum remote target
    cat <<EOF
Upload to $HOST:$D/launcher: tetrad-launch.sh, tetrad-launcher.sh, tetrad-update.sh (INSTALL needs it)
  per file: cat > ….part, md5sum of the .part; only a matching .part is chmod 755 and
  moved into place. Nothing is armed and /etc/init.d/S80drvinit is not changed.
EOF
    confirm
    qc "mkdir -p $D/launcher" || die "mkdir $D/launcher failed"
    for name in tetrad-launch.sh tetrad-launcher.sh tetrad-update.sh; do
        target=$D/launcher/$name
        sum=$(md5sum "$REPO/scripts/launcher/$name" | cut -d' ' -f1)
        qc "cat > $target.part" <"$REPO/scripts/launcher/$name" || die "copy of $name failed"
        remote=$(qc "md5sum $target.part") || die "md5sum of $target.part failed"
        if [[ ${remote%% *} != "$sum" ]]; then
            qc "rm -f $target.part" || true
            die "$target.part md5 ${remote%% *} is not $sum; not moved"
        fi
        qc "chmod 755 $target.part && mv $target.part $target" || die "mv of $name failed"
        echo "tetrad-host: $target md5 $sum"
    done
}

command=${1:-help}
shift || true
case $command in
    upload | presets | stage | stop | logs | pull | launcher) "$command" "$@" ;;
    *) sed -n '2,19p' "${BASH_SOURCE[0]}" ;;
esac
