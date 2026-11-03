#!/usr/bin/env bash
# Build and install c/moq, then build the C probe against the installed package through CMake
# and run it. Outside Windows, also builds and runs it through pkg-config. A source build
# installs to lib64/, as some distros do, so find_package and pkg-config are proven to honor
# CMAKE_INSTALL_LIBDIR; archives use lib/. CC picks the compiler on Unix; Windows uses MSVC.
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

cd "$(dirname "${BASH_SOURCE[0]}")/../../c"

# A source build checks formatting first; the release jobs that pass an input
# check a build of sources already gated.
if [[ -z "$archive" && -z "$ffi_dir" ]]; then
    bash ../sh/clang-format.sh --check c
fi

# Single-config generators build Debug. Visual Studio ignores CMAKE_BUILD_TYPE and builds the
# package with --config Release, since the Rust staticlib links the release CRT (/MD) either way.
build=moq/build
libdir=lib64
if [[ -n "$ffi_dir" ]]; then build=moq/build/prebuilt; fi
if [[ -n "$archive" ]]; then
    build=moq/build/archive
    libdir=lib
    version=$(tr -d '[:space:]' <moq/VERSION)
    target=$(rustc -vV | sed -n 's/^host: //p')
    name="moq-c-$version-$target"
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
    if [[ ! -f "$prefix/lib/cmake/moq-c/moq-c-config.cmake" ]]; then
        echo "Missing moq-c package in $archive" >&2
        exit 1
    fi
else
    prefix="$PWD/$build/prefix"
    cmake -S moq -B "$build/package" -DCMAKE_BUILD_TYPE=Debug -DCMAKE_INSTALL_LIBDIR="$libdir" -DMOQ_FFI_DIR="$ffi_dir"
    cmake --build "$build/package" --config Release
    rm -rf "$prefix"
    cmake --install "$build/package" --config Release --prefix "$prefix"
fi

# Ninja refuses two rules for one output, so a dry run proves both packages share one cargo rule.
if [[ -z "$archive" && -z "$ffi_dir" ]] && command -v ninja >/dev/null 2>&1; then
    cmake -G Ninja -S moq/test/superbuild -B "$build/superbuild" -DCMAKE_BUILD_TYPE=Debug
    ninja -C "$build/superbuild" -n >/dev/null
fi

# Naming the system makes CMake cross-compile, which a source build of moq-ffi refuses at
# configure, pointing at a prebuilt input. Windows' generator is left alone.
if [[ -z "$archive" && -z "$ffi_dir" && "$(uname -s)" != MINGW* && "$(uname -s)" != MSYS* ]]; then
    if cmake -S moq -B "$build/cross" -DCMAKE_SYSTEM_NAME="$(uname -s)" >"$build/cross.log" 2>&1; then
        echo "A cross-compiling source build configured instead of refusing" >&2
        exit 1
    fi
    grep -q MOQ_FFI_DIR "$build/cross.log" || {
        cat "$build/cross.log" >&2
        exit 1
    }
    # A target directory named like a triple is still a host build, and reuses cargo's output.
    target_dir=$(cargo metadata --locked --no-deps --format-version 1 | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
    mkdir -p "$build/odd"
    ln -sfn "$target_dir" "$build/odd/aarch64-unknown-linux-gnu"
    CARGO_TARGET_DIR="$PWD/$build/odd/aarch64-unknown-linux-gnu" cmake --build "$build/package" --target moq_ffi_cargo
fi

run() {
    if [[ -x "$1/probe" ]]; then "$1/probe"; else "$1/${2:-Release}/probe.exe"; fi
}
# Debian-family CMake never searches lib64/ under a prefix, so a lib64 install is found by its
# config directory, as a Debian user of one would have to.
find=(-DCMAKE_PREFIX_PATH="$prefix")
[[ "$libdir" == lib ]] || find=(-Dmoq-c_DIR="$prefix/$libdir/cmake/moq-c")
# An MSVC consumer links the release runtime (/MD) in every configuration, as the docs say,
# since the package's C++ and Rust objects do.
cmake -S moq/test -B "$build/test" -DCMAKE_BUILD_TYPE=Debug \
    -DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL "${find[@]}"
cmake --build "$build/test" --config Release
run "$build/test"
# Visual Studio also builds Debug, the configuration whose default runtime (/MDd) mismatches.
if [[ ! -x "$build/test/probe" ]]; then
    cmake --build "$build/test" --config Debug
    run "$build/test" Debug
fi

# The rest builds with `cc`, which on Windows is MinGW GCC and can't link the MSVC-built
# package. MSVC consumers use the CMake package.
if [[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* ]]; then
    exit 0
fi

if command -v pkg-config >/dev/null 2>&1; then
    export PKG_CONFIG_PATH="$prefix/$libdir/pkgconfig"
    # Assigned first: set -e ignores a failing substitution inside a command's arguments.
    flags=$(pkg-config --cflags --libs moq-c)
    mkdir -p "$build/pkg-config"
    # shellcheck disable=SC2086 # flags is a flag list.
    "${CC:-cc}" -std=c11 -Wall -Wextra -Werror -pedantic-errors -o "$build/pkg-config/probe" moq/test/probe.c $flags
    run "$build/pkg-config"
fi
