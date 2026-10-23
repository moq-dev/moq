#!/usr/bin/env bash
# Build and install cpp/moq, then build the probe against the installed package
# at C++17, at C++23, and at C++23 over bindings at C++17 through CMake, and run
# each. Outside Windows, also builds and runs it through pkg-config at C++17
# and compiles the doc samples and benchmark runner. The bindings compile once
# per standard, and every consumer at that standard links them. CXX picks the
# compiler on Unix; Windows uses MSVC.
set -euo pipefail

ffi_dir=""
archive=""
if [[ $# -gt 0 ]]; then
    if [[ $# != 2 || ("$1" != --ffi-dir && "$1" != --archive) ]]; then
        echo "Usage: $0 [--ffi-dir DIR | --archive FILE]" >&2
        exit 1
    fi
    if [[ "$1" == --archive ]]; then
        archive="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
        [[ -f "$archive" ]] || {
            echo "Archive not found: $archive" >&2
            exit 1
        }
    else
        ffi_dir="$(cd "$2" && pwd)"
    fi
fi

cd "$(dirname "${BASH_SOURCE[0]}")/../../cpp"

# Single-config generators build the package Debug, for cargo's dev profile. The consumers
# clear the build type, since debug info only slows the bindings' compile and link. Visual
# Studio ignores CMAKE_BUILD_TYPE and gets --config Release, since the Rust staticlib links
# the release CRT (/MD) either way.
build=moq/build
if [[ -n "$ffi_dir" ]]; then build=moq/build/prebuilt; fi
if [[ -n "$archive" ]]; then
    build=moq/build/archive
    version=$(tr -d '[:space:]' <moq/VERSION)
    target=$(rustc -vV | sed -n 's/^host: //p')
    name="moq-cpp-$version-$target"
    extension=tar.gz
    [[ "$target" == *-windows-* ]] && extension=zip
    if [[ "$(basename "$archive")" != "$name.$extension" ]]; then
        echo "Expected $name.$extension, got $archive" >&2
        exit 1
    fi
    prefix="$PWD/$build/package/$name"
    rm -rf "$build/package"
    mkdir -p "$build/package"
    (cd "$build/package" && cmake -E tar xf "$archive")
    if [[ ! -f "$prefix/lib/cmake/moq-cpp/moq-cpp-config.cmake" ]]; then
        echo "Missing moq-cpp package in $archive" >&2
        exit 1
    fi
else
    prefix="$PWD/$build/prefix"
    cmake -S moq -B "$build/package" -DCMAKE_BUILD_TYPE=Debug -DCMAKE_INSTALL_LIBDIR=lib -DMOQ_FFI_DIR="$ffi_dir"
    cmake --build "$build/package" --config Release
    rm -rf "$prefix"
    cmake --install "$build/package" --config Release --prefix "$prefix"
fi

run() {
    if [[ -x "$1/$2" ]]; then "$1/$2"; else "$1/Release/$2.exe"; fi
}
for std in 17 23; do
    cmake -S moq/test -B "$build/test-$std" -DCMAKE_BUILD_TYPE= -DCMAKE_CXX_STANDARD="$std" -DCMAKE_PREFIX_PATH="$prefix"
    cmake --build "$build/test-$std" --config Release --parallel
    run "$build/test-$std" probe
done
# The C++23 probe over the C++17 bindings.
run "$build/test-17" probe-cxx23

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
    # Those sources already compiled at C++17 with these flags in test-17, so the probe links
    # that library instead of compiling them again.
    if [[ ! -f "$sources" ]]; then
        echo "pkg-config sources not found: $sources" >&2
        exit 1
    fi
    mkdir -p "$build/pkg-config"
    # shellcheck disable=SC2086 # flags is a flag list.
    "${CXX:-c++}" -std=c++17 -fno-exceptions -fno-rtti -o "$build/pkg-config/probe" moq/test/probe.cpp \
        "$build/test-17/libmoq-cpp.a" $flags
    run "$build/pkg-config" probe
fi
