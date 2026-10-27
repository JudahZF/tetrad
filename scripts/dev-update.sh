#!/bin/sh
# Your own Tetrad updates (docs/development.md, "Your own updates"). The unit installs only a
# manifest that the key built into its Tetrad signed. This makes your own key, and signs the
# files of a build with it, so that the Remote page (a dev build) or `tetrad-usb install` can
# send them. It does what the release workflow does, with your key.
#
#   dev-update.sh key KEY.pem
#       Makes the private key KEY.pem when it is not there, and prints the public key. Build
#       Tetrad with it: TETRAD_RELEASE_PUBKEY=<public key> cargo ... (or in build-arm.sh's
#       environment). Keep KEY.pem; nobody else needs it.
#   dev-update.sh pack DIR KEY.pem
#       DIR holds the `tetrad` binary of the build. Adds the three launcher scripts,
#       tetrad-update.json (the version of Cargo.toml, and the size and SHA-256 of each file) and
#       tetrad-update.json.sig (its Ed25519 signature). Send the six files.
#   dev-update.sh archive BINARY [--scripts] [OUT]
#       Makes OUT (default tetrad-update.tar.gz, beside BINARY): one file with the `tetrad`
#       binary, and with --scripts the three launcher scripts of this checkout. This is what a
#       dev build takes over Wi-Fi: unsigned, so it needs no key. scripts/build-dev.sh makes it
#       after each build.
#   dev-update.sh push ARCHIVE HOST PIN [--install]
#       Sends the files of ARCHIVE to the Remote page of a dev build, as the page does: HOST is
#       the IP address of the unit, PIN the one that Settings > System > Remote shows. Touch
#       UPDATE TETRAD on the unit after it, or give --install to start the install from here
#       (the unit checks the files, installs them and restarts).
#   dev-update.sh install HOST PIN
#       Starts the install of the files that a push left on the unit, as UPDATE TETRAD does.
#   dev-update.sh status HOST PIN
#       Prints the version of the running Tetrad and the md5 that the launcher has armed
#       (launcher/select): after an update, the md5 of the binary that was sent.
#   dev-update.sh authorize HOST PIN KEY.pem
#       Gives the unit the public key of KEY.pem (dev-update.sh key makes one), with the PIN that
#       the unit shows now. Once.
#   dev-update.sh log HOST PIN
#       Prints the recent notices of the unit (the log), over the same Remote page of a dev
#       build.
#
# Where a command takes PIN, `key:KEY.pem` works too, once the key is known to the unit
# (authorize): the unit gives a one-time challenge, the key signs it, and the unit answers with
# its PIN. The key never leaves your computer, and the PIN is always the one of this start.
#
# `key` and `pack` are for builds with their own signing key, which `tetrad-usb install` takes
# over USB; `archive` and `push` need no key.
# Needs OpenSSL 3 (key, pack), base64, sha256sum or shasum (pack), tar, and curl (push).
set -eu

here=$(cd -- "$(dirname -- "$0")" && pwd)
repo=$(cd -- "$here/.." && pwd)
scripts="tetrad-launch.sh tetrad-launcher.sh tetrad-update.sh"

fail() { echo "dev-update: $*" >&2; exit 1; }
usage() { sed -n '2,45p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }

sha256() {
    if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -c1-64
    else shasum -a 256 "$1" | cut -c1-64; fi
}
size() { wc -c <"$1" | tr -d ' '; }
b64() { base64 | tr -d '\n'; }

public_key() {
    openssl pkey -in "$1" -pubout -outform DER | tail -c 32 | b64
}

# One request to the Remote page: call METHOD PATH [BODY-FILE]. The body of the answer is in
# $reply; a status other than 200 stops with the reason that the unit gives. A POST always has a
# body, empty when there is no file: the server wants Content-Length on every POST.
call() {
    data=
    [ "$1" != POST ] || data=@${3:-/dev/null}
    status=$(curl -sS -m 30 -o "$reply" -w '%{http_code}' -X "$1" -H "X-Tetrad-Pin: $pin" \
        ${data:+--data-binary "$data"} "http://$host$2") || {
        echo "dev-update: the unit at $host is not reachable" >&2
        exit 75
    }
    if [ "$status" != 200 ]; then
        fail "$2: $status $(sed -n 's/.*"error":"\([^"]*\)".*/\1/p' "$reply")"
    fi
}

# A PIN of the form key:KEY.pem is fetched with that key (see above); $reply must be set.
resolve_pin() {
    case $pin in key:*) ;; *) return 0 ;; esac
    keyfile=${pin#key:}
    [ -f "$keyfile" ] || fail "no $keyfile"
    pin=
    call GET /api/auth/nonce
    nonce=$(sed -n 's/.*"nonce":"\([^"]*\)".*/\1/p' "$reply")
    [ -n "$nonce" ] || fail "no challenge from the unit"
    message=$(mktemp)
    printf '%s%s' "tetrad-remote-pin:" "$nonce" >"$message"
    signature=$(openssl pkeyutl -sign -inkey "$keyfile" -rawin -in "$message" | b64)
    rm -f "$message"
    printf '{"key":"%s","nonce":"%s","signature":"%s"}' "$(public_key "$keyfile")" "$nonce" \
        "$signature" >"$reply.body"
    call POST /api/auth/pin "$reply.body"
    rm -f "$reply.body"
    pin=$(sed -n 's/.*"pin":"\([^"]*\)".*/\1/p' "$reply")
    [ -n "$pin" ] || fail "no PIN from the unit"
}

case ${1:-} in
    key)
        [ $# -eq 2 ] || usage
        key=$2
        if [ ! -f "$key" ]; then
            (umask 077 && openssl genpkey -algorithm ed25519 -out "$key") || fail "cannot make $key"
            echo "made $key (keep it private)" >&2
        fi
        pub=$(public_key "$key")
        echo "public key: $pub"
        echo "build with: TETRAD_RELEASE_PUBKEY=$pub"
        ;;
    pack)
        [ $# -eq 3 ] || usage
        dir=$2
        key=$3
        [ -f "$dir/tetrad" ] || fail "no $dir/tetrad (build it first)"
        [ -f "$key" ] || fail "no $key (dev-update.sh key $key makes it)"
        version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml")
        [ -n "$version" ] || fail "no version in $repo/Cargo.toml"
        for s in $scripts; do
            install -m 755 "$repo/scripts/launcher/$s" "$dir/$s"
        done
        # The files of a manifest in sorted order: the binary, then the scripts.
        entry() { printf '"%s":{"size":%s,"sha256":"%s"}' "$1" "$(size "$2")" "$(sha256 "$2")"; }
        {
            printf '{"version":"%s","files":{' "$version"
            entry bin/tetrad "$dir/tetrad"
            for s in $scripts; do
                printf ','
                entry "launcher/$s" "$dir/$s"
            done
            printf '}}'
        } >"$dir/tetrad-update.json"
        openssl pkeyutl -sign -inkey "$key" -rawin -in "$dir/tetrad-update.json" | b64 \
            >"$dir/tetrad-update.json.sig"
        echo "tetrad $version: signed with the key $(public_key "$key")" >&2
        echo "send these files of $dir: tetrad $scripts tetrad-update.json tetrad-update.json.sig" >&2
        ;;
    authorize)
        [ $# -eq 4 ] || usage
        host=$2
        pin=$3
        keyfile=$4
        command -v curl >/dev/null || fail "authorize needs curl"
        [ -f "$keyfile" ] || fail "no $keyfile (dev-update.sh key $keyfile makes it)"
        reply=$(mktemp)
        trap 'rm -f "$reply" "$reply.body"' EXIT
        printf '{"key":"%s"}' "$(public_key "$keyfile")" >"$reply.body"
        call POST /api/keys "$reply.body"
        echo "the unit knows the key now; use key:$keyfile where the PIN goes" >&2
        ;;
    status)
        [ $# -eq 3 ] || usage
        host=$2
        pin=$3
        command -v curl >/dev/null || fail "status needs curl"
        reply=$(mktemp)
        trap 'rm -f "$reply"' EXIT
        resolve_pin
        call GET /api/update
        echo "version: $(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' "$reply")"
        echo "armed:   $(sed -n 's/.*"select":"\([^"]*\)".*/\1/p' "$reply")"
        ;;
    log)
        [ $# -eq 3 ] || usage
        host=$2
        pin=$3
        command -v curl >/dev/null || fail "log needs curl"
        reply=$(mktemp)
        trap 'rm -f "$reply"' EXIT
        resolve_pin
        call GET /api/log
        cat "$reply"
        ;;
    archive)
        [ $# -ge 2 ] || usage
        binary=$2
        shift 2
        with_scripts=
        out=
        for arg in "$@"; do
            case $arg in
                --scripts) with_scripts=yes ;;
                *) [ -z "$out" ] || usage; out=$arg ;;
            esac
        done
        [ -f "$binary" ] || fail "no $binary"
        [ -n "$out" ] || out=$(cd -- "$(dirname -- "$binary")" && pwd)/tetrad-update.tar.gz
        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT
        install -m 755 "$binary" "$tmp/tetrad"
        names=tetrad
        if [ -n "$with_scripts" ]; then
            for s in $scripts; do
                install -m 755 "$repo/scripts/launcher/$s" "$tmp/$s"
            done
            names="tetrad $scripts"
        fi
        # shellcheck disable=SC2086 # names is a list of file names without spaces
        tar -C "$tmp" --owner=0 --group=0 --numeric-owner -czf "$out" $names
        echo "$out: $names ($(size "$out") bytes)" >&2
        ;;
    install)
        [ $# -eq 3 ] || usage
        host=$2
        pin=$3
        command -v curl >/dev/null || fail "install needs curl"
        reply=$(mktemp)
        trap 'rm -f "$reply"' EXIT
        resolve_pin
        call POST /api/update/install
        echo "the install started; the unit restarts when it is done" >&2
        ;;
    push)
        [ $# -eq 4 ] || [ $# -eq 5 ] || usage
        archive=$2
        host=$3
        pin=$4
        install_now=
        if [ $# -eq 5 ]; then
            [ "$5" = --install ] || usage
            install_now=yes
        fi
        command -v curl >/dev/null || fail "push needs curl"
        [ -f "$archive" ] || fail "no $archive"
        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT
        reply=$tmp/reply
        chunk=$tmp/chunk
        mkdir "$tmp/files"
        tar -C "$tmp/files" -xzf "$archive" || fail "$archive is not a tar.gz"
        # The four update files by their base names; the unit takes no other.
        list=
        for name in tetrad $scripts; do
            [ -f "$tmp/files/$name" ] || continue
            case $name in tetrad) path=bin/tetrad ;; *) path=launcher/$name ;; esac
            list="$list $path"
        done
        [ -n "$list" ] || fail "$archive has none of: tetrad $scripts"
        resolve_pin
        call GET /api/update
        echo "the unit runs Tetrad $(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' "$reply")" >&2
        sizes=
        for path in $list; do
            sizes="$sizes${sizes:+,}\"$path\":$(size "$tmp/files/${path##*/}")"
        done
        printf '{"files":{%s}}' "$sizes" >"$tmp/begin"
        # One attempt: the files, in pieces of $chunk_size bytes (at most 65536, what the unit
        # takes), from a new begin, which starts the staging over. A piece that the network drops
        # ends the attempt (the subshell), and the next attempt cuts the files differently:
        # a drop can depend on where a piece starts, and this one then does not repeat.
        attempt() {
            call POST /api/update/begin "$tmp/begin"
            for path in $list; do
                file=$tmp/files/${path##*/}
                total=$(size "$file")
                offset=0
                while [ "$offset" -lt "$total" ]; do
                    dd if="$file" of="$chunk" bs="$chunk_size" skip=$((offset / chunk_size)) count=1 2>/dev/null
                    call POST "/api/update/chunk?path=$(echo "$path" | sed 's|/|%2F|')&offset=$offset" "$chunk"
                    offset=$((offset + $(size "$chunk")))
                    printf '\r%s %s/%s' "${path##*/}" "$offset" "$total" >&2
                done
                echo >&2
            done
            call POST /api/update/ready
        }
        staged=
        for chunk_size in ${TETRAD_CHUNK:-49152 40000 32768 24576}; do
            if (attempt); then
                staged=yes
                break
            else
                code=$?
            fi
            # 75: the network dropped it. Anything else is the unit saying no, which again will not
            # change.
            [ "$code" = 75 ] || exit "$code"
            echo "push: that attempt was dropped; starting over with pieces of the next size" >&2
        done
        [ -n "$staged" ] || fail "the files could not be sent"
        if [ -n "$install_now" ]; then
            call POST /api/update/install
            echo "staged; the install started and the unit restarts when it is done" >&2
        else
            echo "staged; touch UPDATE TETRAD on the unit (Settings > System > Tetrad Updates)" >&2
        fi
        ;;
    *) usage ;;
esac
