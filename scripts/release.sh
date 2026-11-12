#!/bin/sh
# The version and manifest steps of the release workflow (.github/workflows/release.yml). The
# contract that units read is in docs/releases.md.
#
#   release.sh version
#       Prints the version in Cargo.toml ([workspace.package]).
#   release.sh nightly-version VERSION DATE TIME SHA
#       Prints the nightly version of a build of commit SHA at DATE (YYYYMMDD) and TIME (HHMM),
#       in UTC, when Cargo.toml has VERSION: X.Y.Z-nightly.DATE.TIME+SHA. When VERSION is a
#       pre-release (0.2.0-alpha.1), X.Y.Z is its core (0.2.0); when it is stable (0.1.0), the
#       patch goes up one (0.1.1). So a nightly is newer than VERSION and older than the next
#       stable.
#   release.sh stamp VERSION
#       Writes VERSION into Cargo.toml and, with `cargo update --workspace`, into the workspace
#       entries of Cargo.lock, so that `cargo build --locked` works and the binary reports
#       VERSION (CARGO_PKG_VERSION). Stops when another line of Cargo.lock changes.
#   release.sh manifest VERSION CHANNEL DIR
#       DIR holds the `tetrad` binary. Adds the three launcher scripts and tetrad-update.json:
#       the version and the size and SHA-256 of each file. CHANNEL is `stable` or `nightly`.
#       Only a nightly manifest has the key "channel": units up to 0.1.0-alpha.10 refuse a
#       manifest with a key they do not know.
# Needs sed, sha256sum, jq (manifest), and cargo and git (stamp).
set -eu

here=$(cd -- "$(dirname -- "$0")" && pwd)
repo=$(cd -- "$here/.." && pwd)
scripts="tetrad-launch.sh tetrad-launcher.sh tetrad-update.sh"

fail() { echo "release: $*" >&2; exit 1; }
usage() { sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }

cargo_version() {
    sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml"
}

nightly_version() {
    cargo=$1
    case $2 in [1-9][0-9][0-9][0-9][0-1][0-9][0-3][0-9]) ;; *) fail "date '$2' is not YYYYMMDD" ;; esac
    case $3 in [0-2][0-9][0-5][0-9]) ;; *) fail "time '$3' is not HHMM" ;; esac
    case $4 in '' | *[!0-9a-f]*) fail "commit '$4' is not lowercase hex" ;; esac
    core=${cargo%%[-+]*}
    case $core in
        *[!0-9.]* | *.*.*.* | .* | *. | *..*) fail "version '$cargo' is not X.Y.Z[-PRE]" ;;
        *.*.*) ;;
        *) fail "version '$cargo' is not X.Y.Z[-PRE]" ;;
    esac
    case ${cargo%%+*} in
        *-*) base=$core ;;
        *) base=${core%.*}.$((${core##*.} + 1)) ;;
    esac
    # A numeric semver identifier has no leading zero: 0905 is written 905.
    time=$(printf '%s' "$3" | sed 's/^0*\(.\)/\1/')
    echo "$base-nightly.$2.$time+$4"
}

stamp() {
    sed '/^\[workspace.package\]/,/^\[/s/^version = ".*"$/version = "'"$1"'"/' \
        "$repo/Cargo.toml" >"$repo/Cargo.toml.new"
    mv -- "$repo/Cargo.toml.new" "$repo/Cargo.toml"
    [ "$(cargo_version)" = "$1" ] || fail "Cargo.toml does not have version $1"
    (cd -- "$repo" && cargo update --workspace --quiet)
    # Only the version lines of the workspace crates may change: a nightly builds the locked
    # dependencies of its commit.
    other=$(git -C "$repo" diff -U0 -- Cargo.lock | grep '^[-+][^-+]' | grep -v '^[-+]version = "' || true)
    [ -z "$other" ] || fail "cargo update changed more than versions in Cargo.lock: $other"
}

entry() {
    jq -n --arg path "$1" --arg sha256 "$(sha256sum "$2" | cut -c1-64)" \
        --argjson size "$(wc -c <"$2" | tr -d ' ')" '{($path): {size: $size, sha256: $sha256}}'
}

manifest() {
    version=$1 channel=$2 dir=$3
    case $channel in stable | nightly) ;; *) fail "channel '$channel' is not stable or nightly" ;; esac
    [ -f "$dir/tetrad" ] || fail "no tetrad binary in $dir"
    for s in $scripts; do
        cp -- "$here/launcher/$s" "$dir/$s"
        chmod 755 "$dir/$s"
    done
    {
        entry bin/tetrad "$dir/tetrad"
        for s in $scripts; do entry "launcher/$s" "$dir/$s"; done
    } | jq -s --arg version "$version" --arg channel "$channel" \
        '{version: $version}
         + (if $channel == "stable" then {} else {channel: $channel} end)
         + {files: add}' >"$dir/tetrad-update.json"
}

[ $# -ge 1 ] || usage
command=$1
shift
case $command in
    version) [ $# -eq 0 ] || usage; cargo_version ;;
    nightly-version) [ $# -eq 4 ] || usage; nightly_version "$@" ;;
    stamp) [ $# -eq 1 ] || usage; stamp "$1" ;;
    manifest) [ $# -eq 3 ] || usage; manifest "$@" ;;
    *) usage ;;
esac
