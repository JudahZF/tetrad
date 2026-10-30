#!/bin/sh
# Set-up of a Cursor cloud agent VM (Ubuntu 24.04), run by .cursor/environment.json from the
# repository root at each build and start. It must be idempotent. It gives what CI has: the
# submodules, Rust 1.95.0 with Clippy and rustfmt, gh, the published extensions in
# $HOME/tetrad-extensions (for TETRAD_EXTENSIONS_DIR) and a warm build cache.
set -eu

git submodule update --init --recursive

missing=
for tool in curl jq gh; do
    command -v "$tool" >/dev/null || missing="$missing $tool"
done
command -v cc >/dev/null || missing="$missing build-essential"
if [ -n "$missing" ]; then
    sudo apt-get update -qq
    # shellcheck disable=SC2086 # one word per package
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $missing
fi

if [ ! -x "$HOME/.cargo/bin/rustup" ] && ! command -v rustup >/dev/null; then
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
fi
PATH="$HOME/.cargo/bin:$PATH"
rustup toolchain install 1.95.0 --profile minimal --component clippy,rustfmt
rustup default 1.95.0

scripts/fetch-extensions.sh "$HOME/tetrad-extensions"
cargo build --locked --workspace --all-targets
