#!/usr/bin/env bash
# Build and install cpp/moq, then build the probe against the installed package
# at C++17, at C++23, and at C++23 over bindings at C++17 through CMake, and run
# each. Outside Windows, also builds and runs it through pkg-config at C++17
# and compiles the doc samples and benchmark runner. CXX picks the compiler on
# Unix; Windows uses MSVC.
set -euo pipefail

ffi_dir=""
if [[ $# -gt 0 ]]; then
    if [[ $# != 2 || "$1" != --ffi-dir ]]; then
        echo "Usage: $0 [--ffi-dir DIR]" >&2
        exit 1
    fi
    ffi_dir="$(cd "$2" && pwd)"
fi

cd "$(dirname "${BASH_SOURCE[0]}")/../../cpp"

# Single-config generators build Debug. Visual Studio ignores CMAKE_BUILD_TYPE and gets
# --config Release, since the Rust staticlib links the release CRT (/MD) either way.
build=moq/build
if [[ -n "$ffi_dir" ]]; then build=moq/build/prebuilt; fi
prefix="$PWD/$build/prefix"
cmake -S moq -B "$build/package" -DCMAKE_BUILD_TYPE=Debug -DCMAKE_INSTALL_LIBDIR=lib -DMOQ_FFI_DIR="$ffi_dir"
cmake --build "$build/package" --config Release
rm -rf "$prefix"
cmake --install "$build/package" --config Release --prefix "$prefix"

run() {
    if [[ -x "$1/probe" ]]; then "$1/probe"; else "$1/Release/probe.exe"; fi
}
for std in 17 23; do
    cmake -S moq/test -B "$build/test-$std" -DCMAKE_BUILD_TYPE=Debug -DCMAKE_CXX_STANDARD="$std" -DCMAKE_PREFIX_PATH="$prefix"
    cmake --build "$build/test-$std" --config Release
    run "$build/test-$std"
done

# Bindings at C++17 under an app target raised to C++23: moq::expected is one type at every
# standard, so the two link.
cmake -S moq/test -B "$build/test-mixed" -DCMAKE_BUILD_TYPE=Debug -DCMAKE_CXX_STANDARD=17 -DPROBE_CXX_STANDARD=23 -DCMAKE_PREFIX_PATH="$prefix"
cmake --build "$build/test-mixed" --config Release
run "$build/test-mixed"

# The rest builds with `c++`, which on Windows is MinGW GCC and can't link the MSVC-built
# package. MSVC consumers use the CMake package.
if [[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* ]]; then
    exit 0
fi

# The samples in doc/lib/cpp compile against the installed headers.
bash ../doc/lib/samples.sh cpp ../doc/lib/cpp/index.md >"$build/doc-samples.cpp"
"${CXX:-c++}" -std=c++17 -fsyntax-only -Wall -Werror -include moq/test/doc-samples.hpp \
    -I"$prefix/include" "$build/doc-samples.cpp"

# The nightly runs the binding benchmark; compiling it here catches wrapper drift per PR.
"${CXX:-c++}" -std=c++20 -fsyntax-only -Wall -Werror -I"$prefix/include" moq/bench/main.cpp

if command -v pkg-config >/dev/null 2>&1; then
    export PKG_CONFIG_PATH="$prefix/lib/pkgconfig"
    # Assigned first: set -e ignores a failing substitution inside a command's arguments.
    sources=$(pkg-config --variable=sources moq-cpp)
    flags=$(pkg-config --cflags --libs moq-cpp)
    mkdir -p "$build/pkg-config"
    # shellcheck disable=SC2086 # flags is a flag list.
    "${CXX:-c++}" -std=c++17 -o "$build/pkg-config/probe" moq/test/probe.cpp "$sources" $flags
    run "$build/pkg-config"
fi
