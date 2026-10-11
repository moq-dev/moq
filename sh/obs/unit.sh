#!/usr/bin/env bash
# Compile and run the unit tests: sh/obs/unit.sh [plain|tsan]
#
# Shared by `just obs test` (ThreadSanitizer) and `just obs ci` (plain), so
# both run the same assertions.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$here/../../cpp/obs"
mode=${1:-plain}

cxx="${CXX:-c++}"
if ! command -v "$cxx" >/dev/null; then
    echo "no C++ compiler at '$cxx'" >&2
    exit 1
fi
# moq-source.cpp decodes with ffmpeg, so its test needs the headers even
# though it stubs every function out of them.
if ! pkg-config --exists libavcodec libavutil libswscale; then
    echo "missing ffmpeg headers" >&2
    echo "run inside 'nix develop', which supplies ffmpeg on every platform" >&2
    exit 1
fi

includes_raw=$("$here/includes.sh")
includes=()
while IFS= read -r flag; do includes+=("$flag"); done <<<"$includes_raw"

ffmpeg=$(pkg-config --cflags libavcodec libavutil libswscale)

# The output and source tests run against the real moq-ffi, over a server in
# the same process. The bindings compile here, with the tests' flags.
moq_prefix=$("$here/moq.sh")
export PKG_CONFIG_PATH="$moq_prefix/lib/pkgconfig"
moq_sources=$(pkg-config --variable=sources moq-cpp)
moq_libs=$(pkg-config --libs moq-cpp)

flags=(-std=c++17 -g -O0 -pthread)
if [ "$mode" = "tsan" ]; then
    flags+=(-fsanitize=thread)
fi

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT

"$cxx" "${flags[@]}" "${includes[@]}" -c "$moq_sources" -o "$out/moq.o"

# One binary per source under test: each test file defines its own libobs
# stubs, so two of them can't share a link. Header-only helpers (quality
# defaults) build as separate binaries with no plugin source.
for name in moq-output moq-source obs-moq; do
    # shellcheck disable=SC2086
    # $ffmpeg and $moq_libs stay unquoted on purpose: pkg-config hands back
    # one space-separated string, and splitting it is the intended reading.
    "$cxx" "${flags[@]}" "${includes[@]}" $ffmpeg \
        -o "$out/$name-test" "test/$name-test.cpp" "src/$name.cpp" "$out/moq.o" $moq_libs
    TSAN_OPTIONS="halt_on_error=1 suppressions=$PWD/test/tsan.supp" "$out/$name-test"
done
for name in moq-quality-defaults moq-dock-stop moq-error moq-encoder-latency moq-dial moq-spark; do
    "$cxx" "${flags[@]}" "${includes[@]}" \
        -o "$out/$name-test" "test/$name-test.cpp"
    TSAN_OPTIONS="halt_on_error=1 suppressions=$PWD/test/tsan.supp" "$out/$name-test"
done
