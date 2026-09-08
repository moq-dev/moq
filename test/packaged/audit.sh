#!/usr/bin/env bash
# Refuse a publishable crate whose path dependency carries no version.
#
# This is the one packaging defect a workspace build can never surface: inside
# the workspace the path resolves and everything compiles, and `cargo package`
# is the first thing that asks what version a consumer would resolve instead.
# Cargo does refuse, but from inside a `Packaging` step whose message reads as a
# cargo internal, so name the crate and the dependency here first.
#
#     audit.sh <workspace-manifest> <crate>...
#
# Split from rust.sh so the negative control can point it at a disposable
# workspace instead of this repo's.
set -euo pipefail

MANIFEST="${1:?usage: audit.sh <workspace-manifest> <crate>...}"
shift

metadata=$("${RUST_CARGO:-cargo}" metadata --no-deps --format-version 1 --manifest-path "$MANIFEST")

failed=0
for name in "$@"; do
    unversioned=$(jq -r --arg name "$name" '
		.packages[]
		| select(.name == $name)
		| .dependencies[]
		| select(.kind == null or .kind == "build")
		| select(.path != null and .req == "*")
		| .name
	' <<<"$metadata")
    [[ -n "$unversioned" ]] || continue
    failed=1
    while read -r dep; do
        echo "packaged: $name: path dependency $dep has no version; it cannot be published" >&2
    done <<<"$unversioned"
done

exit "$failed"
