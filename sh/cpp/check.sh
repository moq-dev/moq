#!/usr/bin/env bash
# Build and install cpp/moq, then build the probe against the installed package
# at C++17 and C++23 through CMake (C++23 also the documented in-project way) and
# run each. Outside Windows, also builds and runs it through pkg-config at C++17
# and compiles the doc samples. CXX picks the compiler on Unix; Windows uses MSVC.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../cpp"

# Single-config generators build Debug. Visual Studio ignores CMAKE_BUILD_TYPE and gets
# --config Release, since the Rust staticlib links the release CRT (/MD) either way.
build=moq/build
prefix="$PWD/$build/prefix"
cmake -S moq -B "$build/package" -DCMAKE_BUILD_TYPE=Debug -DCMAKE_INSTALL_LIBDIR=lib
cmake --build "$build/package" --config Release
rm -rf "$prefix"
cmake --install "$build/package" --config Release --prefix "$prefix"

# Every generated moq::MoqFoo needs its moq::Foo alias in the wrapper.
missing=$(comm -23 \
    <(sed -nE 's/^(struct|enum class) Moq([A-Za-z0-9]+);$/\2/p' "$prefix/include/moq/ffi/moq.hpp" | sort -u) \
    <(sed -nE 's/^using ([A-Za-z0-9]+) = Moq([A-Za-z0-9]+);$/\1 \2/p' moq/include/moq/moq.hpp |
        awk '$1 == $2 { print $1 }' | sort -u))
if [[ -n "$missing" ]]; then
    echo "cpp check: cpp/moq/include/moq/moq.hpp lacks an alias for: ${missing//$'\n'/ }" >&2
    exit 1
fi

run() {
    if [[ -x "$1/probe" ]]; then "$1/probe"; else "$1/Release/probe.exe"; fi
}
for std in 17 23; do
    cmake -S moq/test -B "$build/test-$std" -DCMAKE_BUILD_TYPE=Debug -DCMAKE_CXX_STANDARD="$std" -DCMAKE_PREFIX_PATH="$prefix"
    cmake --build "$build/test-$std" --config Release
    run "$build/test-$std"
done

# The setup doc/lib/cpp documents: the standard set in the project, and also required on
# the app's own target.
cmake -S moq/test -B "$build/test-target-23" -DCMAKE_BUILD_TYPE=Debug -DPROBE_CXX_STANDARD=23 -DCMAKE_PREFIX_PATH="$prefix"
cmake --build "$build/test-target-23" --config Release
run "$build/test-target-23"

# The rest builds with `c++`, which on Windows is MinGW GCC and can't link the MSVC-built
# package. MSVC consumers use the CMake package.
if [[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* ]]; then
    exit 0
fi

# The samples in doc/lib/cpp compile against the installed headers.
bash ../doc/lib/samples.sh cpp ../doc/lib/cpp/index.md >"$build/doc-samples.cpp"
"${CXX:-c++}" -std=c++17 -fsyntax-only -Wall -Werror -include moq/test/doc-samples.hpp \
    -I"$prefix/include" "$build/doc-samples.cpp"

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
