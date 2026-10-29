#!/usr/bin/env bash
# Write the FFI interfaces of the last release and of this checkout as headers,
# so sh/api-diff/run.sh can diff them. usage: sh/api-diff/ffi.sh <out>
#
# - moq.h: the C header, from the newest `moq-c-v*` (formerly `libmoq-v*`)
#   release archive, and from moq-c's build script here.
# - moq-ffi.hpp: the UniFFI surface every binding is generated from, rendered as
#   the C++ header uniffi-bindgen-cpp writes for it, which declares each exported
#   item with its full signature. The baseline comes from the newest `moq-ffi-v*`
#   release's Linux library, which carries the UniFFI metadata.
#
# The preprocessor strips comments from both, so a doc edit is not a change.
# <out>/surfaces.tsv lists each surface, its baseline version, and its file.
set -euo pipefail

out=${1:?usage: sh/api-diff/ffi.sh <out>}
cd "$(git rev-parse --show-toplevel)"
mkdir -p "$out/baseline" "$out/current"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The newest release tag matching a pattern; `gh release list` sorts newest first.
latest() {
    gh release list --repo moq-dev/moq --limit 1000 --json tagName --jq "[.[].tagName | select(test(\"$1\"))][0] // empty"
}

# Strip comments without expanding includes or macros. Only GCC can: Clang has
# no -fpreprocessed, and is what `cc` and even `gcc` name on macOS.
gcc --version 2>/dev/null | grep -q 'Free Software Foundation' || {
    echo "api-diff: the FFI diff needs GCC to strip header comments; run \`just api-diff rust js\` without it" >&2
    exit 1
}
strip() {
    gcc -x "$1" -fpreprocessed -dD -E -P -w "$2" -o "$3"
}

c_tag=$(latest '^(moq-c|libmoq)-v')
ffi_tag=$(latest '^moq-ffi-v')
if [[ -z "$c_tag" || -z "$ffi_tag" ]]; then
    echo "no moq-c or moq-ffi release found" >&2
    exit 1
fi

# The header is the same in every archive; take the Linux x86_64 one.
gh release download "$c_tag" --repo moq-dev/moq --pattern '*-x86_64-unknown-linux-gnu.tar.gz' --dir "$work/c"
tar -xzf "$work"/c/*.tar.gz -C "$work/c" --wildcards '*/include/moq.h'
strip c "$work"/c/*/include/moq.h "$out/baseline/moq.h"

# moq-c's build script writes the header into its OUT_DIR, which `cargo check` runs.
c_out=$(cargo check --locked -p moq-c --message-format=json |
    jq -r 'select(.reason == "build-script-executed") | select(.package_id | test("/moq-c#")) | .out_dir' | tail -1)
if [[ -z "$c_out" || ! -f "$c_out/include/moq.h" ]]; then
    echo "cargo check -p moq-c wrote no moq.h" >&2
    exit 1
fi
strip c "$c_out/include/moq.h" "$out/current/moq.h"

# uniffi-bindgen-cpp reads the metadata from a library for any target, so the
# x86_64 release library works on any host.
gh release download "$ffi_tag" --repo moq-dev/moq --pattern '*-x86_64-unknown-linux-gnu-libmoq_ffi.so' --output "$work/libmoq_ffi.so"
lib=$(cargo build-ffi --lib --message-format=json |
    jq -r 'select(.reason == "compiler-artifact" and .target.name == "moq_ffi") | .filenames[] | select(test("\\.(so|dylib)$"))' | tail -1)
if [[ -z "$lib" ]]; then
    echo "cargo build -p moq-ffi produced no shared library" >&2
    exit 1
fi

for side in baseline current; do
    source=$work/libmoq_ffi.so
    [[ "$side" == baseline ]] || source=$lib
    uniffi-bindgen-cpp --library "$source" --config cpp/moq/uniffi.toml --out-dir "$work/cpp-$side" >&2
    # Named after the UniFFI namespace; the other headers are generator plumbing.
    strip c++ "$work/cpp-$side/moq.hpp" "$out/$side/moq-ffi.hpp"
done

printf 'moq.h\t%s\tmoq.h\nmoq-ffi (UniFFI)\t%s\tmoq-ffi.hpp\n' "${c_tag##*-v}" "${ffi_tag##*-v}" >"$out/surfaces.tsv"
