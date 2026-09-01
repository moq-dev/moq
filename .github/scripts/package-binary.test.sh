#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

binary="$tmp/moq-relay"
printf '#!/usr/bin/env sh\necho test\n' >"$binary"
chmod 0755 "$binary"
target="$(rustc -vV | awk '/^host:/ {print $2}')"

# This test covers archive staging, not the macOS Mach-O rewrite.
mkdir "$tmp/bin"
printf '#!/usr/bin/env sh\nprintf "Linux\\n"\n' >"$tmp/bin/uname"
chmod 0755 "$tmp/bin/uname"

PATH="$tmp/bin:$PATH" "$WORKSPACE_DIR/rs/scripts/package-binary.sh" \
    --crate moq-relay \
    --bin moq-relay \
    --binary "$binary" \
    --bare \
    --version 0.14.13 \
    --target "$target" \
    --output "$tmp/dist"

name="moq-relay-v0.14.13-$target"
bare="$tmp/dist/$name"
archive="$tmp/dist/$name.tar.gz"

[[ -x "$bare" ]]
[[ -f "$archive" ]]

mkdir "$tmp/extracted"
tar -xzf "$archive" -C "$tmp/extracted"
[[ -x "$tmp/extracted/$name/bin/moq-relay" ]]
[[ -f "$tmp/extracted/$name/LICENSE-MIT" ]]
[[ -f "$tmp/extracted/$name/LICENSE-APACHE" ]]
cmp "$binary" "$bare"
cmp "$binary" "$tmp/extracted/$name/bin/moq-relay"

echo "release assets package together without path collisions"
