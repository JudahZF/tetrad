#!/bin/sh
# install.sh: install, update, check, turn off or remove Tetrad on your own Quad Cortex.
# It runs on your computer (Linux or macOS, POSIX sh) and drives the unit over root SSH.
# Guide: docs/install.md. Run `scripts/install.sh --help` for the commands and options.
#
# On the unit it writes only /media/p4/tetrad and, through `tetrad-launcher.sh install-hook`
# and `remove-hook`, the md5-pinned edit of /etc/init.d/S80drvinit. It never writes
# /opt/neuraldsp or a partition. `install` and `update` also move an installation of zen-rs
# (Tetrad's old working name, /media/p4/zenui-rs) to /media/p4/tetrad (see `migrate`).
set -eu

D=/media/p4/tetrad
L=$D/launcher
TOOL="sh $L/tetrad-launcher.sh"
SCRIPTS='tetrad-launch.sh tetrad-launcher.sh tetrad-update.sh'
# zen-rs, the old working name: its folder and the md5 of S80drvinit with its hook.
OLD=/media/p4/zenui-rs
OLD_HOOKED_MD5=d6bafded21b2192dc18927f575fbb742
ROOT=$(cd "$(dirname "$0")/.." && pwd)

usage() {
    cat <<'EOF'
Usage: scripts/install.sh [OPTIONS] COMMAND

Commands:
  install    Upload Tetrad, add the boot hook, select Tetrad, restart the unit.
  update     Replace the Tetrad binary and launcher scripts, select the new binary, restart.
  status     Show the hook, the selected binary and the UI that runs now. Changes nothing.
  disable    Stock CorOS from the next boot on (the hook stays). Restarts the unit.
  enable     Tetrad from the next boot on (adds the hook again if it is gone). Restarts.
  uninstall  Turn off Tetrad and remove the boot hook. Asks to delete /media/p4/tetrad.

Options:
  --host HOST     SSH host: IP, name, user@host or ssh config alias (default: $QC_HOST)
  --port PORT     SSH port (default: your ssh config, else 22)
  --ssh-opt OPT   extra ssh option, given to ssh as "-o OPT" (repeat for more)
  --proxy         send the SSH banner slowly (scripts/qc-ssh-proxy.py, needs python3)
  --binary PATH   the ARM tetrad binary (default: bin/tetrad of a release archive, else
                  dist/tetrad of qc-dev/build-arm.sh)
  --no-reboot     do not restart the unit at the end
  --yes           uninstall: also delete /media/p4/tetrad, with no question
  --dry-run       print the remote commands; connect to nothing
  -h, --help      show this help
EOF
}

die() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

say() {
    printf '%s\n' "$*"
}

# One word, single-quoted for `eval`.
quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

need_value() {
    [ $# -ge 2 ] || die "$1 needs a value (see --help)"
}

HOST=${QC_HOST:-}
PORT=
EXTRA=
BINARY=
REBOOT=1
YES=0
DRY_RUN=0
PROXY=0
CMD=
while [ $# -gt 0 ]; do
    case $1 in
        --host) need_value "$@"; HOST=$2; shift 2 ;;
        --port) need_value "$@"; PORT=$2; shift 2 ;;
        --ssh-opt) need_value "$@"; EXTRA="$EXTRA -o $(quote "$2")"; shift 2 ;;
        --binary) need_value "$@"; BINARY=$2; shift 2 ;;
        --proxy) PROXY=1; shift ;;
        --no-reboot) REBOOT=0; shift ;;
        --yes) YES=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h | --help) usage; exit 0 ;;
        -*) die "unknown option $1 (see --help)" ;;
        *) [ -z "$CMD" ] || die "give one command only (see --help)"; CMD=$1; shift ;;
    esac
done

case $CMD in
    install | update | status | disable | enable | uninstall) ;;
    '') usage >&2; exit 2 ;;
    *) die "unknown command $CMD (see --help)" ;;
esac
[ -n "$HOST" ] || die "no unit given: use --host HOST or set QC_HOST"
case $HOST in -* | *[[:space:]]*) die "'$HOST' is not a host name" ;; esac
case $PORT in *[!0-9]*) die "--port must be a number" ;; esac

# One SSH connection for all steps (ControlMaster), so you type a password once at most.
# Log in as root unless HOST names a user; your ssh config gives the rest.
SSH_ARGS="-T -o ConnectTimeout=20 -o ServerAliveInterval=5 -o ServerAliveCountMax=3"
SSH_ARGS="$SSH_ARGS -o ControlMaster=auto -o ControlPath=/tmp/tetrad-install-%C -o ControlPersist=600"
case $HOST in *@*) ;; *) SSH_ARGS="$SSH_ARGS -l root" ;; esac
[ -z "$PORT" ] || SSH_ARGS="$SSH_ARGS -p $PORT"
if [ "$PROXY" = 1 ]; then
    proxy=$ROOT/scripts/qc-ssh-proxy.py
    [ -f "$proxy" ] || die "--proxy needs $proxy"
    command -v python3 >/dev/null 2>&1 || die "--proxy needs python3"
    SSH_ARGS="$SSH_ARGS -o $(quote "ProxyCommand=python3 \"$proxy\" %h %p")"
fi
SSH_ARGS="$SSH_ARGS$EXTRA"
[ "$DRY_RUN" = 0 ] || say "[dry-run] each step runs: ssh $SSH_ARGS $HOST '<command>'"

# $1: a command for the unit's shell (BusyBox ash 1.23.1). stdin is the caller's.
ssh_run() {
    if [ "$DRY_RUN" = 1 ]; then
        printf '[dry-run] ssh %s:\n%s\n' "$HOST" "$1" | sed '2,$s/^/    /'
        return 0
    fi
    eval "ssh $SSH_ARGS \"\$HOST\" \"\$1\""
}

remote() {
    ssh_run "$1" </dev/null
}

# $1: a step name for the error, $2: a remote command. Exit 255 is an SSH failure.
step() {
    code=0
    remote "$2" || code=$?
    case $code in
        0) ;;
        255) die "$1: the SSH connection failed (see docs/install.md, 'SSH problems')" ;;
        *) die "$1: failed on the unit (exit $code); see the messages above" ;;
    esac
}

NEED_REBOOT=0
UI_STOPPED=0

reboot_unit() {
    say "Restart the unit (/opt/neuraldsp/zenreboot). A UI shows again in about one minute."
    code=0
    remote /opt/neuraldsp/zenreboot || code=$?
    case $code in
        0 | 255) ;;
        *) say "install.sh: zenreboot exit $code; restart the unit with its power switch" >&2 ;;
    esac
}

# Runs on every exit, also after a failed step: a stopped UI or a change must not stay
# without a restart unless you asked for that with --no-reboot.
on_exit() {
    if [ "$NEED_REBOOT" = 1 ]; then
        if [ "$REBOOT" = 1 ]; then
            reboot_unit || true
        else
            [ "$UI_STOPPED" = 0 ] || say "install.sh: no UI runs on the unit now."
            say "Restart the unit to apply this: ssh root@<unit> /opt/neuraldsp/zenreboot"
        fi
    fi
    [ "$DRY_RUN" = 1 ] || eval "ssh $SSH_ARGS -O exit \"\$HOST\"" >/dev/null 2>&1 || true
}
trap on_exit EXIT
trap 'exit 130' INT TERM HUP

local_md5() {
    if command -v md5sum >/dev/null 2>&1; then
        md5sum <"$1" | cut -c1-32
    else
        md5 -q "$1"
    fi
}

# The local files for install and update: the binary and the launcher scripts. Sets BINARY,
# LDIR, BIN_MD5 and <script>_MD5 values through md5_of_script.
find_files() {
    if [ -z "$BINARY" ]; then
        for b in "$ROOT/bin/tetrad" "$ROOT/dist/tetrad"; do
            [ -f "$b" ] && { BINARY=$b; break; }
        done
        [ -n "$BINARY" ] || die "no tetrad binary found; give one with --binary PATH"
    fi
    [ -f "$BINARY" ] || die "no file at $BINARY"
    # ELF, 32-bit, machine 0x28 (ARM): a host build would fail on the unit.
    elf=$(od -An -tx1 -N20 "$BINARY" | tr -d ' \n')
    case $elf in
        7f454c4601*) [ "$(printf '%s' "$elf" | cut -c37-38)" = 28 ] || die "$BINARY is not an ARM binary" ;;
        *) die "$BINARY is not an ARM Linux binary" ;;
    esac
    LDIR=
    for d in "$ROOT/launcher" "$ROOT/scripts/launcher"; do
        [ -f "$d/tetrad-launch.sh" ] && { LDIR=$d; break; }
    done
    [ -n "$LDIR" ] || die "no launcher scripts in $ROOT/launcher or $ROOT/scripts/launcher"
    for s in $SCRIPTS; do
        [ -f "$LDIR/$s" ] || die "no file at $LDIR/$s"
    done
    BIN_MD5=$(local_md5 "$BINARY")
}

# The S80drvinit md5 values that tetrad-launcher.sh accepts, read from the copy to be uploaded.
hook_pins() {
    STOCK_MD5=$(sed -n 's/^TD_INIT_STOCK_MD5=\([0-9a-f]\{32\}\).*/\1/p' "$LDIR/tetrad-launcher.sh")
    HOOKED_MD5=$(sed -n 's/^TD_INIT_HOOKED_MD5=\([0-9a-f]\{32\}\).*/\1/p' "$LDIR/tetrad-launcher.sh")
    [ -n "$STOCK_MD5" ] && [ -n "$HOOKED_MD5" ] || die "no S80drvinit md5 pins in $LDIR/tetrad-launcher.sh"
}

# Read-only checks: root, a Quad Cortex, p4 mounted.
check_unit() {
    step "check the unit" "$(cat <<EOF
[ "\$(id -u)" = 0 ] || { echo 'qc: not root; log in as root' >&2; exit 1; }
[ -x /opt/neuraldsp/ZenUI ] || { echo 'qc: no /opt/neuraldsp/ZenUI; this is not a Quad Cortex' >&2; exit 1; }
[ -x /opt/neuraldsp/zenreboot ] || { echo 'qc: no /opt/neuraldsp/zenreboot' >&2; exit 1; }
grep -q ' /media/p4 ' /proc/mounts || { echo 'qc: /media/p4 is not mounted' >&2; exit 1; }
echo 'qc: Quad Cortex found, root login, /media/p4 mounted'
$1
EOF
)"
}

INSTALLED="[ -f $L/tetrad-launcher.sh ] || { echo 'qc: Tetrad is not installed (no $L/tetrad-launcher.sh)' >&2; [ ! -d $OLD ] || echo 'qc: $OLD is zen-rs; run scripts/install.sh update to move it' >&2; exit 1; }"

# Sets MIGRATE=1 when the unit has the zen-rs folder. A dry run assumes it, to show the steps.
find_old_layout() {
    MIGRATE=0
    code=0
    remote "[ -d $OLD ]" || code=$?
    case $code in
        0)
            MIGRATE=1
            [ "$DRY_RUN" = 0 ] || say "[dry-run] the next steps include the move of zen-rs ($OLD)"
            ;;
        1) ;;
        255) die "check for $OLD: the SSH connection failed (see docs/install.md, 'SSH problems')" ;;
        *) die "check for $OLD failed on the unit (exit $code)" ;;
    esac
}

# Stop ZenWatchdog first (it restarts the unit 3 s after the UI goes), then the UI (stock or
# Tetrad, both named ZenUI): SIGTERM, then SIGKILL after 5 s. Stock's process killer stops
# with stock, so the uploads and `tetrad run --check` are not killed.
stop_ui() {
    UI_STOPPED=1
    NEED_REBOOT=1
    step "stop ZenWatchdog and the UI" "$(cat <<'EOF'
w=$(pidof ZenWatchdog)
if [ -n "$w" ]; then
    kill $w; i=0
    while [ $i -lt 20 ] && pidof ZenWatchdog >/dev/null; do usleep 100000; i=$((i + 1)); done
    w=$(pidof ZenWatchdog); [ -z "$w" ] || kill -9 $w
    usleep 200000
fi
if pidof ZenWatchdog >/dev/null; then echo 'qc: ZenWatchdog does not stop' >&2; exit 1; fi
p=$(pidof ZenUI)
if [ -n "$p" ]; then
    kill $p; i=0
    while [ $i -lt 50 ] && pidof ZenUI >/dev/null; do usleep 100000; i=$((i + 1)); done
    p=$(pidof ZenUI); [ -z "$p" ] || kill -9 $p
    usleep 200000
fi
if pidof ZenUI >/dev/null; then echo 'qc: the UI does not stop' >&2; exit 1; fi
echo 'qc: ZenWatchdog and the UI are stopped'
EOF
)"
}

# zen-rs to Tetrad, with the UI stopped: the old launcher on the unit removes its own hook (the
# result must be the stock file), then $OLD becomes $D. The logs and the launcher state stay
# (boot-choice, no-chooser, launch.log); the old binary, scripts, `select` and `pending` go.
# install and update then upload, hook and arm as usual. Needs STOCK_MD5 (hook_pins).
migrate() {
    say "Move zen-rs ($OLD) to $D"
    step "move zen-rs to $D" "$(cat <<EOF
[ ! -e $D ] || { echo 'qc: $OLD and $D both exist; remove one of them, then run this again' >&2; exit 1; }
case \$(md5sum </etc/init.d/S80drvinit | cut -c1-32) in
    $OLD_HOOKED_MD5)
        sh $OLD/launcher/zr-launcher.sh remove-hook || exit 1
        [ "\$(md5sum </etc/init.d/S80drvinit | cut -c1-32)" = $STOCK_MD5 ] ||
            { echo 'qc: S80drvinit is not the stock file after remove-hook' >&2; exit 1; }
        ;;
    $STOCK_MD5) echo 'qc: S80drvinit has no zen-rs hook' ;;
    *) echo 'qc: /etc/init.d/S80drvinit is not a known file; refused' >&2; exit 1 ;;
esac
mv $OLD $D || exit 1
cd $D || exit 1
rm -f bin/zenui-rs bin/zr-stage.sh launcher/zr-launch.sh launcher/zr-launcher.sh \\
    launcher/zr-update.sh launcher/select launcher/select.tmp launcher/pending launcher/pending.tmp
sync
echo 'qc: $OLD is now $D; logs, boot-choice, no-chooser and launch.log kept'
EOF
)"
}

# $1 local file, $2 remote path, $3 md5. Writes $2.part (mode 755) and keeps it only when its
# md5 is $3.
put() {
    say "  $1 -> $2 ($(wc -c <"$1" | tr -d ' ') bytes, md5 $3)"
    code=0
    ssh_run "$(cat <<EOF
cat >$2.part || { rm -f $2.part; echo 'qc: cannot write $2.part (is /media/p4 full?)' >&2; exit 1; }
s=\$(md5sum <$2.part | cut -c1-32)
[ "\$s" = $3 ] || { rm -f $2.part; echo "qc: $2.part has md5 \$s, not $3; removed" >&2; exit 1; }
chmod 755 $2.part
EOF
)" <"$1" || code=$?
    case $code in
        0) ;;
        255) die "upload of $1: the SSH connection failed" ;;
        *) die "upload of $1 failed (exit $code); nothing was replaced" ;;
    esac
}

# install and update: upload every file as .part, check them all, then move them into place.
push_files() {
    step "make the folders" "mkdir -p $D/bin $L && rm -f $D/bin/*.part $L/*.part"
    say "Upload (the binary takes about one minute over Wi-Fi):"
    put "$BINARY" "$D/bin/tetrad" "$BIN_MD5"
    for s in $SCRIPTS; do
        put "$LDIR/$s" "$L/$s" "$(local_md5 "$LDIR/$s")"
    done
    say "Check the new files on the unit:"
    step "launcher syntax" "for s in $SCRIPTS; do sh -n $L/\$s.part || { echo \"qc: \$s does not parse\" >&2; exit 1; }; done; echo 'qc: the launcher scripts parse'"
    code=0
    remote "cd /opt/neuraldsp && $D/bin/tetrad.part run --check" || code=$?
    if [ "$code" != 0 ]; then
        remote "rm -f $D/bin/*.part $L/*.part" || true
        case $code in
            4) die "this Tetrad does not support the CorOS build on the unit (supported: 4.1.0, 4.1.1); nothing was replaced" ;;
            126 | 127) die "the binary does not start on the unit (exit $code); is it the ARM release build?" ;;
            255) die "tetrad run --check: the SSH connection failed" ;;
            *) die "tetrad run --check failed (exit $code); nothing was replaced" ;;
        esac
    fi
    step "move the files into place" "cd $D && for f in bin/tetrad $(for s in $SCRIPTS; do printf 'launcher/%s ' "$s"; done); do mv -f \$f.part \$f || exit 1; done && sync && echo 'qc: files in place'"
}

case $CMD in
    install)
        find_files
        hook_pins
        find_old_layout
        old_hook=
        [ "$MIGRATE" = 0 ] || old_hook="$OLD_HOOKED_MD5) echo 'qc: S80drvinit has the zen-rs hook' ;;"
        say "[1/6] Check the unit"
        check_unit "$(cat <<EOF
case \$(md5sum </etc/init.d/S80drvinit | cut -c1-32) in
    $STOCK_MD5) echo 'qc: S80drvinit is the stock CorOS file' ;;
    $HOOKED_MD5) echo 'qc: S80drvinit has the Tetrad hook already' ;;
    $old_hook
    *) echo 'qc: /etc/init.d/S80drvinit is not a known CorOS 4.1.0 or 4.1.1 file; refused' >&2; exit 1 ;;
esac
EOF
)"
        say "[2/6] Stop ZenWatchdog and the UI (the screen stays as it is until the restart)"
        stop_ui
        [ "$MIGRATE" = 0 ] || migrate
        say "[3/6] Upload and check Tetrad"
        push_files
        say "[4/6] Add the boot hook to /etc/init.d/S80drvinit"
        step "install-hook" "$TOOL install-hook"
        say "[5/6] Select this Tetrad binary"
        step "arm" "$TOOL arm"
        say "[6/6] Status"
        step "status" "$TOOL status"
        say "Done. After the restart the boot selector shows for 5 s; Tetrad starts unless you touch CorOS."
        ;;
    update)
        find_files
        find_old_layout
        say "[1/5] Check the unit"
        if [ "$MIGRATE" = 1 ]; then
            hook_pins
            check_unit "[ -f $OLD/launcher/zr-launcher.sh ] || { echo 'qc: no $OLD/launcher/zr-launcher.sh' >&2; exit 1; }"
        else
            check_unit "$INSTALLED"
        fi
        say "[2/5] Stop ZenWatchdog and the UI"
        stop_ui
        [ "$MIGRATE" = 0 ] || migrate
        say "[3/5] Upload and check Tetrad"
        push_files
        # The move removed the zen-rs hook; add the Tetrad one.
        [ "$MIGRATE" = 0 ] || step "install-hook" "$TOOL install-hook"
        say "[4/5] Select the new binary"
        step "arm" "$TOOL arm"
        say "[5/5] Status"
        step "status" "$TOOL status"
        say "Done. If 'hook:' above is not 'hooked', run: scripts/install.sh enable"
        ;;
    status)
        check_unit ''
        step "status" "$(cat <<EOF
if [ -f $L/tetrad-launcher.sh ]; then $TOOL status; else echo 'Tetrad:   not installed'; fi
ui=none
for p in \$(pidof ZenUI); do
    case \$(ls -l /proc/\$p/exe 2>/dev/null) in
        *tetrad*) ui=Tetrad ;;
        *ZenUI*) [ \$ui = Tetrad ] || ui='stock CorOS' ;;
    esac
done
echo "running:  \$ui"
EOF
)"
        ;;
    disable)
        check_unit "$INSTALLED"
        NEED_REBOOT=1
        step "disarm" "$TOOL disarm"
        ;;
    enable)
        check_unit "$INSTALLED"
        step "install-hook" "$TOOL install-hook"
        NEED_REBOOT=1
        step "arm" "$TOOL arm"
        ;;
    uninstall)
        check_unit "$INSTALLED"
        NEED_REBOOT=1
        step "disarm" "$TOOL disarm"
        step "remove-hook" "$TOOL remove-hook"
        delete=$YES
        if [ "$YES" = 0 ] && [ "$DRY_RUN" = 0 ] && [ -t 0 ]; then
            printf 'Also delete %s on the unit (binary, launcher, logs, boot choice)? [y/N] ' "$D"
            read -r answer || answer=
            case $answer in y | Y | yes) delete=1 ;; esac
        fi
        if [ "$delete" = 1 ]; then
            step "delete $D" "rm -rf $D && sync && echo 'qc: $D deleted'"
        elif [ "$DRY_RUN" = 1 ]; then
            say "[dry-run] with --yes or a 'y' answer: rm -rf $D"
        else
            say "Kept $D. S80drvinit is the stock file again; without the hook nothing runs these files."
        fi
        ;;
esac
