#!/usr/bin/env bash
# Build the moq C++ package from the in-tree cpp/moq, install it to a prefix,
# and print the prefix. Rebuilt every time rather than trusted from a previous
# run: the headers are rendered from the moq-ffi build, so a stale copy would
# type-check the plugin against an API that has since changed. Cargo and the
# copy-if-different render keep a no-op rebuild cheap.
#
# Debug, for the same reason `just obs ci` is: the Rust cache CI restores holds
# dev-profile artifacts.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../cpp/moq"

if ! command -v uniffi-bindgen-cpp >/dev/null 2>&1; then
    echo "uniffi-bindgen-cpp not on PATH; run inside 'nix develop', which supplies it" >&2
    exit 1
fi
build="$PWD/build/obs"
prefix="$build/prefix"
{
    cmake -S . -B "$build" -DCMAKE_BUILD_TYPE=Debug -DCMAKE_INSTALL_LIBDIR=lib
    cmake --build "$build" --config Release
    cmake --install "$build" --config Release --prefix "$prefix"
} >&2
echo "$prefix"
