#!/usr/bin/env bash
# The C compiler sh/rs/cross-lint.sh hands to a build script for a macOS or
# Windows target; sh/rs/zig-c++.sh and sh/rs/zig-ar.sh are its C++ compiler and
# archiver. zig carries the libc headers of every target it supports, so no
# Apple or Windows SDK is needed. cc-rs adds the target as Apple's or LLVM's
# flags, which zig rejects in favour of its own, taken here from the TARGET
# Cargo gives the build script.
set -euo pipefail

mode=${ZIG_CC_MODE:-cc}

case "${TARGET:?Cargo sets TARGET for a build script}" in
    aarch64-apple-darwin) target=aarch64-macos ;;
    # MinGW's headers stand in for the MSVC SDK, which no free package
    # carries. A lint links nothing, so the objects' ABI never matters.
    # cc-rs archives for MSVC with lib.exe's arguments.
    x86_64-pc-windows-msvc)
        target=x86_64-windows-gnu
        [[ "$mode" != ar ]] || mode=lib
        ;;
    *)
        echo "zig-cc: no zig target for $TARGET" >&2
        exit 1
        ;;
esac

case "$mode" in
    ar | lib) exec zig "$mode" "$@" ;;
esac

args=()
skip=
for arg; do
    if [[ -n "$skip" ]]; then
        skip=
        continue
    fi
    case "$arg" in
        -arch) skip=1 ;;
        -mmacosx-version-min=* | --target=*) ;;
        *) args+=("$arg") ;;
    esac
done

exec zig "$mode" -target "$target" ${args[@]+"${args[@]}"}
