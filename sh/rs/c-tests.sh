#!/usr/bin/env bash
# Compile and run libmoq's C fixtures (`rs/libmoq/c-tests/*.c`) against
# libmoq.a, linked the way an embedder does: an external `cc` against the
# generated `moq.h`, plus the native libraries from `rs/libmoq/native-libs`
# that cargo can't inject into a link it doesn't drive. Unix only.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

cc="${CC:-cc}"
if ! command -v "$cc" >/dev/null; then
    echo "no C compiler at '$cc'" >&2
    exit 1
fi

cargo build --locked -p libmoq

# Ask cargo where it put the staticlib rather than assuming target/debug:
# CARGO_TARGET_DIR and a configured build target (`target/<triple>/debug`)
# both move it, and guessing wrong would link a stale copy or nothing.
# Replays the build above from cache, so it costs a process, not a compile.
lib=$(cargo build --locked -p libmoq --message-format=json |
    jq -r 'select(.reason == "compiler-artifact") | .filenames[] | select(endswith("/libmoq.a"))' |
    tail -1)
if [ -z "$lib" ]; then
    echo "cargo reported no libmoq.a for libmoq" >&2
    exit 1
fi
# The header sits beside the profile directory: rs/libmoq/build.rs writes it
# to `<target>/include`, one level above `<target>/<profile>/libmoq.a`.
profile=$(dirname "$lib")
include="$(dirname "$profile")/include"
if [ ! -f "$include/moq.h" ]; then
    echo "$include/moq.h missing after 'cargo build --locked -p libmoq'" >&2
    exit 1
fi

# Same list build.rs (moq.pc), CMakeLists.txt, and test/interop/interop.sh read:
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
for source in rs/libmoq/c-tests/*.c; do
    bin="$out/$(basename "$source" .c)"
    "$cc" "$source" -I"$include" -L"$profile" -lmoq "${libs[@]}" -o "$bin"
    echo "running $source"
    "$bin"
done
