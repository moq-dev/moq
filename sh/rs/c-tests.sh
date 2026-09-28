#!/usr/bin/env bash
# Compile and run libmoq's C fixtures (`rs/libmoq/c-tests/*.c`), and the C doc
# samples, against libmoq.a, linked the way an embedder does: an external `cc`
# against the generated `moq.h`, plus the native libraries from
# `rs/libmoq/native-libs` that cargo can't inject into a link it doesn't drive.
# Unix only.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

cc="${CC:-cc}"
if ! command -v "$cc" >/dev/null; then
    echo "no C compiler at '$cc'" >&2
    exit 1
fi

cargo build --locked -p libmoq

# Ask cargo where it put the staticlib and the header rather than assuming
# target/debug: CARGO_TARGET_DIR and a configured build target
# (`target/<triple>/debug`) both move them, and guessing wrong would link a
# stale copy or nothing. Replays the build above from cache, so it costs a
# process, not a compile.
log=$(cargo build --locked -p libmoq --message-format=json)
lib=$(jq -r 'select(.reason == "compiler-artifact") | .filenames[] | select(endswith("/libmoq.a"))' <<<"$log" | tail -1)
if [ -z "$lib" ]; then
    echo "cargo reported no libmoq.a for libmoq" >&2
    exit 1
fi
profile=$(dirname "$lib")
# rs/libmoq/build.rs writes the header into its OUT_DIR.
out_dir=$(jq -r 'select(.reason == "build-script-executed") | select(.package_id | test("libmoq")) | .out_dir' <<<"$log" | tail -1)
include="$out_dir/include"
if [ -z "$out_dir" ] || [ ! -f "$include/moq.h" ]; then
    echo "$include/moq.h missing after 'cargo build --locked -p libmoq'" >&2
    exit 1
fi

# Same list nix/overlay.nix (moq.pc), CMakeLists.txt, and test/interop/interop.sh read:
# `framework:Foo` is a linker framework flag, anything else a plain library.
case "$(uname -s)" in
    Darwin) native_libs=rs/libmoq/native-libs/apple.txt ;;
    *) native_libs=rs/libmoq/native-libs/linux.txt ;;
esac
libs=()
while read -r entry; do
    case "$entry" in
        '' | '#'*) continue ;;
        framework:*) libs+=(-framework "${entry#framework:}") ;;
        *) libs+=("-l$entry") ;;
    esac
done <"$native_libs"

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT

# The C docs compile against this header too: each sample beside the inputs
# doc-samples.h declares, and every name the prose cites. A `moq_decode_*`
# wildcard ends in `_` and is skipped.
bash doc/lib/samples.sh c doc/lib/c/index.md >"$out/doc-samples.c"
"$cc" -fsyntax-only -Werror=implicit-function-declaration -include rs/libmoq/c-tests/doc-samples.h -I"$include" "$out/doc-samples.c"
grep -oE '\b(moq|MOQ)_[A-Za-z0-9_]*' doc/lib/c/index.md | grep -v '_$' | sort -u | while read -r name; do
    if ! grep -qw "$name" "$include/moq.h"; then
        echo "doc/lib/c/index.md cites $name, which moq.h does not declare" >&2
        exit 1
    fi
done

for source in rs/libmoq/c-tests/*.c; do
    bin="$out/$(basename "$source" .c)"
    "$cc" "$source" -I"$include" -L"$profile" -lmoq "${libs[@]}" -o "$bin"
    echo "running $source"
    "$bin"
done
