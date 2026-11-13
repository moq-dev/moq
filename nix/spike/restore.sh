#!/usr/bin/env bash
# restore.sh: put the baseline's Rust outputs back in the local store from the file caches.
set -euo pipefail
cd "$(dirname "$0")"
read -r B0C B0N < b0
K=$(cat key.pub)
nix copy --from "file://$PWD/cache-crane" --extra-trusted-public-keys "$K" $(./rustpaths.sh "$B0C")
nix copy --from "file://$PWD/cache-c2n" --extra-trusted-public-keys "$K" $(./rustpaths.sh "$B0N")
for d in $B0C $B0N; do for p in $(./rustpaths.sh "$d"); do [ -e "$p" ] || { echo "missing $p"; exit 1; }; done; done
echo restored
