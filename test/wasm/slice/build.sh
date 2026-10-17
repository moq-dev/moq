#!/usr/bin/env bash
# Bind bench/ffi-gen's moq-ffi slice for the browser twice: BoltFFI's web package, and
# UniFFI over ubrn's wasm2 player. Outputs land in slice/dist/{boltffi,uniffi}.
#
# Works on a scratch copy of the slice, so neither BoltFFI's output nor the UniFFI 0.31
# pin (ubrn's version; see ../uniffi/build.sh) touches the tree. See ../README.md.
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$dir/../../.." && pwd)
target="${CARGO_TARGET_DIR:-$root/target}"
out="$target/test/wasm-slice"
slice="$root/bench/ffi-gen"
boltffi_version=0.31.1
ubrn_version=0.31.0-6

[[ -d "$slice" ]] || {
    echo "error: $slice is missing; the slice lands with the BoltFFI spike (#5736)" >&2
    exit 1
}

boltffi="$out/tools/bin/boltffi"
[[ -x "$boltffi" ]] || cargo install --locked --quiet --root "$out/tools" "boltffi_cli@$boltffi_version"
# BoltFFI transpiles its TypeScript with `tsc`, resolved from an ancestor of its output.
[[ -d "$out/node_modules/@boltffi/runtime" ]] ||
    npm install --silent --no-save --prefix "$out" typescript@5 @types/node "@boltffi/runtime@$boltffi_version"
ubrn="$target/test/wasm-uniffi/tools/bin/uniffi-bindgen-react-native"
[[ -x "$ubrn" ]] ||
    cargo install --locked --quiet --root "$target/test/wasm-uniffi/tools" \
        --git https://github.com/jhugman/uniffi-bindgen-react-native --tag "$ubrn_version" uniffi-bindgen-react-native

rm -rf "$out/src" "$dir/dist"
mkdir -p "$out/src" "$dir/dist"
cp -r "$slice/." "$out/src"

# BoltFFI: its CLI builds only `debug` or `release`, so release takes wasm-release's opt-level.
sed -i "s|../../target/ffi-gen/boltffi|$out/boltffi|" "$out/src/boltffi.toml"
(cd "$out/src" && PATH="$out/node_modules/.bin:$PATH" CARGO_TARGET_DIR="$out/cargo/boltffi" \
    CARGO_PROFILE_RELEASE_OPT_LEVEL=z "$boltffi" pack wasm --release --deny-skipped >/dev/null)
cp -r "$out/boltffi/npm/." "$dir/dist/boltffi"
# Where bun resolves the generated module's runtime import from when bundling.
mkdir -p "$dir/dist/node_modules/@boltffi"
cp -r "$out/node_modules/@boltffi/runtime" "$dir/dist/node_modules/@boltffi/"

# UniFFI: the same pin moq-ffi takes, applied to the slice's own manifest.
sed -i 's|uniffi = { version = "0.32"|uniffi = { version = "=0.31"|' "$out/src/Cargo.toml"
sed -i "s|^web-async = .*|&\nuniffi-runtime-wasm = \"=$ubrn_version\"|" "$out/src/Cargo.toml"
printf '\n#[cfg(all(feature = "uniffi", target_arch = "wasm32"))]\nextern crate uniffi_runtime_wasm as _;\n' >>"$out/src/src/lib.rs"
cargo build --quiet --manifest-path "$out/src/Cargo.toml" --profile wasm-release --target wasm32-unknown-unknown \
    --no-default-features --features uniffi --target-dir "$out/cargo/uniffi"
cat >"$out/package.json" <<EOF
{"name": "moq-slice", "version": "0.0.0", "repository": {"type": "git", "url": "https://github.com/moq-dev/moq"}}
EOF
cat >"$out/ubrn.config.yaml" <<EOF
rust:
  directory: $out/src
  manifestPath: Cargo.toml
wasm2:
  ts: $dir/dist/uniffi
  defaultFeatures: false
  features: [uniffi]
EOF
(cd "$out" && CARGO_TARGET_DIR="$out/cargo/uniffi" "$ubrn" build wasm2 --config ubrn.config.yaml --profile wasm-release \
    --no-wasm-build >/dev/null)
