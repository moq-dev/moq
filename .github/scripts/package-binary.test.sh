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

# Without --binary the script builds the flake package named after the binary:
# `.#moq` for the moq-cli crate, since `.#moq-cli` is a stub refusing the old name.
cat >"$tmp/bin/nix" <<'NIX'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $3" == "build --out-link" && "$2" == *"#moq" ]] || {
    echo "unexpected: nix $*" >&2
    exit 1
}
mkdir -p "$4/bin"
printf '#!/usr/bin/env sh\necho moq\n' >"$4/bin/moq"
chmod 0755 "$4/bin/moq"
NIX
chmod 0755 "$tmp/bin/nix"

PATH="$tmp/bin:$PATH" "$WORKSPACE_DIR/rs/scripts/package-binary.sh" \
    --crate moq-cli \
    --bin moq \
    --version 0.12.2 \
    --target "$target" \
    --output "$tmp/dist"

name="moq-cli-v0.12.2-$target"
tar -xzf "$tmp/dist/$name.tar.gz" -C "$tmp/extracted"
[[ "$("$tmp/extracted/$name/bin/moq")" == moq ]]

echo "a nix build packages the flake output named after the binary"
