#!/bin/sh
# Builds a dev build of Tetrad for the Quad Cortex (docs/development.md, "Dev build") and makes
# the update archive that its Remote page takes. Run it in `nix develop`:
#
#   nix develop path:./qc-dev -c ./scripts/build-dev.sh [--scripts] [OUTPUT_DIR]
#
# OUTPUT_DIR (default dist-dev) gets tetrad, tetrad.sha256 and tetrad-update.tar.gz. With
# --scripts the archive also has the launcher scripts of this checkout. TETRAD_RELEASE_PUBKEY in
# the environment gives the build its own signing key (docs/development.md, "Your own updates").
#
# qc-dev/build-arm.sh has no option for features, so a cargo-zigbuild stand-in at the front of
# PATH adds `--features dev` to its `zigbuild` call. Every other step is the release build.
set -eu

here=$(cd -- "$(dirname -- "$0")" && pwd)
repo=$(cd -- "$here/.." && pwd)
build_arm=${TD_BUILD_ARM:-$repo/qc-dev/build-arm.sh}

fail() { echo "build-dev: $*" >&2; exit 1; }

scripts=
out=dist-dev
for arg in "$@"; do
    case $arg in
        --scripts) scripts=--scripts ;;
        -*) fail "unknown option $arg (usage: build-dev.sh [--scripts] [OUTPUT_DIR])" ;;
        *) out=$arg ;;
    esac
done
[ -f "$build_arm" ] || fail "no $build_arm (git submodule update --init)"
real=$(command -v cargo-zigbuild) || fail "missing tool: cargo-zigbuild (run inside 'nix develop path:./qc-dev')"

shim=$(mktemp -d)
trap 'rm -rf "$shim"' EXIT
cat >"$shim/cargo-zigbuild" <<EOF
#!/bin/sh
case "\$1" in
    zigbuild) exec "$real" "\$@" --features dev ;;
    *) exec "$real" "\$@" ;;
esac
EOF
chmod 755 "$shim/cargo-zigbuild"

cd -- "$repo"
PATH="$shim:$PATH" sh "$build_arm" "$repo" "$out"
# shellcheck disable=SC2086 # scripts is empty or one option
sh "$here/dev-update.sh" archive "$out/tetrad" $scripts
echo "send $out/tetrad-update.tar.gz to the Remote page of the unit, or:" >&2
echo "  scripts/dev-update.sh push $out/tetrad-update.tar.gz <unit IP> <PIN>" >&2
