#!/bin/sh
# Downloads the program files of the DOOM extension into app/ and checks their SHA-256.
#
# - qcdoom: the prebuilt qcDOOM binary (GPL-2.0, source at the same commit).
# - doom1.wad: the DOOM shareware WAD v1.9, from Ubuntu's doom-wad-shareware package. id Software
#   lets anyone share it unchanged; the package's copyright file has the statement.
set -eu
cd "$(dirname "$0")"

QCDOOM_URL=https://raw.githubusercontent.com/VanIseghemThomas/qcDOOM/d24ce5a411ade93452dbf213d05b7ead0a17f228/qcdoom/qcdoom
QCDOOM_SHA256=fbb512fdd40ff13146fdb4764bfa65733bd940f74388d19b5eded8d94db59e7f
DEB_URL=http://archive.ubuntu.com/ubuntu/pool/multiverse/d/doom-wad-shareware/doom-wad-shareware_1.9.fixed-2_all.deb
DEB_SHA256=a46bf38d583bc820df9a6662f36f7e5db847cd83700393f8cc5cf374554cf7ca
WAD_SHA256=1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771

check() {
    echo "$2  $1" | sha256sum -c --quiet - || { echo "error: $1 has the wrong SHA-256" >&2; exit 1; }
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/qcdoom" "$QCDOOM_URL"
check "$work/qcdoom" "$QCDOOM_SHA256"
curl -fsSL -o "$work/wad.deb" "$DEB_URL"
check "$work/wad.deb" "$DEB_SHA256"
(cd "$work" && ar x wad.deb data.tar.gz)
tar -xzOf "$work/data.tar.gz" ./usr/share/games/doom/doom1.wad > "$work/doom1.wad"
check "$work/doom1.wad" "$WAD_SHA256"

mkdir -p app
mv "$work/qcdoom" "$work/doom1.wad" app/
echo "app/qcdoom and app/doom1.wad are ready"
