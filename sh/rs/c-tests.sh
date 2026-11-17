#!/usr/bin/env bash
# Compile and run libmoq's C fixtures (`rs/libmoq/c-tests/*.c`), and the C doc
# samples, against libmoq.a, linked the way an embedder does: an external `cc`
# against the generated `moq.h`, plus the native libraries from
# `rs/libmoq/native-libs` that cargo can't inject into a link it doesn't drive.
# Unix only.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

cd "$(git rev-parse --show-toplevel)"

cc="${CC:-cc}"
if ! command -v "$cc" >/dev/null; then
    echo "no C compiler at '$cc'" >&2
    exit 1
fi

cargo build --locked -p libmoq --features openh264

# Ask cargo where it put the staticlib and the header rather than assuming
# target/debug: CARGO_TARGET_DIR and a configured build target
# (`target/<triple>/debug`) both move them, and guessing wrong would link a
# stale copy or nothing. Replays the build above from cache, so it costs a
# process, not a compile.
log=$(cargo build --locked -p libmoq --features openh264 --message-format=json)
lib=$(jq -r 'select(.reason == "compiler-artifact") | .filenames[] | select(endswith("/libmoq.a"))' <<<"$log" | tail -1)
if [ -z "$lib" ]; then
    echo "cargo reported no libmoq.a for libmoq" >&2
    exit 1
fi
profile=$(dirname "$lib")
# rs/libmoq/build.rs writes the header into its OUT_DIR.
out_dir=$(jq -r 'select(.reason == "build-script-executed") | select(.package_id | test("/libmoq#")) | .out_dir' <<<"$log" | tail -1)
include="$out_dir/include"
if [ -z "$out_dir" ] || [ ! -f "$include/moq.h" ]; then
    echo "$include/moq.h missing after 'cargo build --locked -p libmoq --features openh264'" >&2
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

# find_package(moq <version>) takes the same 0.x line only, at least as new as
# the request: render the packed version file the way build.sh and
# nix/overlay.nix do, and ask CMake about each request.
if command -v cmake >/dev/null; then
    pkg="$out/cmake-pkg"
    mkdir -p "$pkg" "$out/cmake-probe"
    sed 's/@VERSION@/0.22.1/' rs/libmoq/cmake/moq-config-version.cmake.in >"$pkg/moq-config-version.cmake"
    echo 'set(moq_FOUND TRUE)' >"$pkg/moq-config.cmake"
    # request:found
    for case in ":1" "0.22:1" "0.22.1:1" "0.22.2:0" "0.23:0" "0.21:0" "0.6:0" "1.0:0" "0.22...0.22.1:1" "0.22...<0.22.1:0" "0.22...<0.23:1" "0.22...0.22.0:0" "0.22...<0.22.2:1"; do
        request="${case%%:*}"
        want="${case##*:}"
        printf 'cmake_minimum_required(VERSION 3.21)\nproject(probe NONE)\nfind_package(moq %s QUIET CONFIG PATHS "%s" NO_DEFAULT_PATH)\nif(moq_FOUND)\n  message(STATUS "found=1")\nelse()\n  message(STATUS "found=0")\nendif()\n' \
            "$request" "$pkg" >"$out/cmake-probe/CMakeLists.txt"
        got=$(cmake -S "$out/cmake-probe" -B "$out/cmake-probe/build" 2>&1 | sed -n 's/.*found=\([01]\).*/\1/p')
        rm -rf "$out/cmake-probe/build"
        if [ "$got" != "$want" ]; then
            echo "find_package(moq $request) against 0.22.1: found=$got, want $want" >&2
            exit 1
        fi
    done
fi
