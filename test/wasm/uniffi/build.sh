#!/usr/bin/env bash
# Build moq-ffi as TypeScript over ubrn's wasm2 player, into uniffi/dist.
#
# ubrn 0.31.0-6 reads UniFFI 0.31 metadata and moq-ffi is on 0.32, so this applies
# pin.patch to a scratch checkout of HEAD rather than to this tree. The pin is for
# measurement only; adoption waits on ubrn's 0.32 support. See ../README.md.
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$dir/../../.." && pwd)
target="${CARGO_TARGET_DIR:-$root/target}"
out="$target/test/wasm-uniffi"
src="$out/src"
ubrn_version=0.31.0-6

# Skip the whole build when neither the commit nor the patch changed since the last one.
stamp="$(git -C "$root" rev-parse HEAD) $(sha256sum <"$dir/pin.patch") $ubrn_version"
if [[ -f "$dir/dist/moq_ffi.wasm" && "$(cat "$out/stamp" 2>/dev/null)" == "$stamp" ]]; then
    exit 0
fi

ubrn="$out/tools/bin/uniffi-bindgen-react-native"
[[ -x "$ubrn" ]] ||
    cargo install --locked --quiet --root "$out/tools" --git https://github.com/jhugman/uniffi-bindgen-react-native \
        --tag "$ubrn_version" uniffi-bindgen-react-native

# A detached worktree of the committed rs/ tree, so the pin never touches this checkout.
git -C "$root" worktree remove --force "$src" 2>/dev/null || rm -rf "$src"
git -C "$root" worktree add --quiet --detach "$src" HEAD
trap 'git -C "$root" worktree remove --force "$src"' EXIT
git -C "$src" apply "$dir/pin.patch"

# Not --locked: the pin re-resolves UniFFI. Its own target dir, so the pinned moq_ffi.wasm
# never stands in for the real one.
cargo build --quiet --manifest-path "$src/Cargo.toml" --package moq-ffi --profile wasm-release \
    --target wasm32-unknown-unknown --target-dir "$out/cargo"

# ubrn wants a package.json and its config beside it; neither belongs in the repo.
cat >"$out/package.json" <<EOF
{"name": "moq-uniffi", "version": "0.0.0", "repository": {"type": "git", "url": "https://github.com/moq-dev/moq"}}
EOF
cat >"$out/ubrn.config.yaml" <<EOF
rust:
  directory: $src
  manifestPath: rs/moq-ffi/Cargo.toml
wasm2:
  ts: $dir/dist
EOF
rm -rf "$dir/dist"
# The wasm-bindgen rewrite ubrn runs must match the lockfile's wasm-bindgen, which the
# dev shell's CLI does as long as the pin leaves it alone.
(cd "$out" && CARGO_TARGET_DIR="$out/cargo" "$ubrn" build wasm2 --config ubrn.config.yaml --profile wasm-release \
    --no-wasm-build >/dev/null)
echo "$stamp" >"$out/stamp"
