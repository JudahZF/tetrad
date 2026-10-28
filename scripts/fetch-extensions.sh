#!/bin/sh
# Downloads a published extension repository into DIR, in the layout `tetrad-ext` reads: the
# catalog, its signature and key, and every package in DIR/packages/, each checked against the
# catalog's SHA-256. Program files of packages with `app` stay in their releases.
#
# - Tests: TETRAD_EXTENSIONS_DIR=DIR cargo test (the published packages must still decode).
# - Publishing: fetch into a clone of the repository, add the new package, then `tetrad-ext
#   catalog` signs a catalog with every package (JudahZF/tetrad-extensions README).
#
# Usage: scripts/fetch-extensions.sh DIR [OWNER/REPO]   (default JudahZF/tetrad-extensions)
# Needs curl, jq and sha256sum.
set -eu

dir=${1:?usage: scripts/fetch-extensions.sh DIR [OWNER/REPO]}
repo=${2:-JudahZF/tetrad-extensions}
mkdir -p "$dir/packages"

for file in tetrad-extensions.json tetrad-extensions.sig tetrad-extensions.pub; do
    curl -fsSL -o "$dir/$file" "https://raw.githubusercontent.com/$repo/HEAD/$file"
done

# Not piped: `set -e` sees a jq failure only outside a pipeline.
assets=$(jq -r '.extensions[].asset | "\(.tag) \(.name) \(.sha256)"' "$dir/tetrad-extensions.json")
count=$(jq '.extensions | length' "$dir/tetrad-extensions.json")
printf '%s\n' "$assets" |
    while read -r tag name sha256; do
        package="$dir/packages/$name"
        if ! echo "$sha256  $package" | sha256sum -c --status - 2>/dev/null; then
            curl -fsSL -o "$package" "https://github.com/$repo/releases/download/$tag/$name"
            echo "$sha256  $package" | sha256sum -c --quiet - ||
                { echo "error: $name does not match the catalog" >&2; exit 1; }
        fi
    done
echo "$repo: $count packages in $dir/packages"
