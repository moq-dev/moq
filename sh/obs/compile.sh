#!/usr/bin/env bash
# Type-check every plugin source and unit test, without linking or an obs-deps
# download.
#
# This is the gate to run in a worktree. `just obs build` needs the
# multi-hundred-MB obs-deps bundle (macOS/Windows) and a full cargo build, per
# worktree; this needs neither, because the dev shell carries all three header
# sets: libobs pinned to the same OBS release buildspec.json downloads (see
# `obs-headers` in flake.nix), Qt6, and ffmpeg. CI links the real thing on Linux
# (.github/workflows/obs.yml).
#
# It compiles the Qt sources too, which the CMake build only does when
# ENABLE_QT and ENABLE_FRONTEND_API are on, so the definitions they gate on are
# set here to match.
#
# test/ is in scope because each test file defines the libmoq entry points the
# plugin calls, so a signature that drifts from the generated moq.h is a
# conflicting C declaration. Catching that needs only headers, which is why it
# belongs here: `just obs ci` finds the same drift, but only where obs.yml's
# path filter reaches, and `just obs test` is manual.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$here/../../cpp/obs"

# Never skips, for the reason `test` doesn't: a compile gate that reports
# success without compiling is worse than no gate. Everything it needs is in
# the dev shell, so the fix is always to enter it.
cxx="${CXX:-c++}"
if ! command -v "$cxx" >/dev/null; then
    echo "no C++ compiler at '$cxx'" >&2
    exit 1
fi
for pkgs in "Qt6Widgets Qt6Gui Qt6Core" "libavcodec libavutil libswscale libswresample"; do
    # shellcheck disable=SC2086
    if ! pkg-config --exists $pkgs; then
        echo "missing headers for: $pkgs" >&2
        echo "run inside 'nix develop', which supplies Qt6 and ffmpeg on every platform" >&2
        exit 1
    fi
done

# Assign first so a failure in `_includes` still aborts under `set -e`,
# then split on newlines only: an include path may contain spaces.
includes_raw=$("$here/includes.sh")
includes=()
while IFS= read -r flag; do includes+=("$flag"); done <<<"$includes_raw"

qt=$(pkg-config --cflags Qt6Widgets Qt6Gui Qt6Core)
ffmpeg=$(pkg-config --cflags libavcodec libavutil libswscale libswresample)

# MOQ_VERSION_STRING only reaches a label in the dock, so any value
# type-checks the same; CMake stamps the real libmoq version.
status=0
for source in src/*.cpp test/*.cpp; do
    # shellcheck disable=SC2086
    # -Wno-unused-command-line-argument: the nix compiler wrapper injects
    # link flags, which a syntax-only run reports as unused, once per file.
    # $qt and $ffmpeg stay unquoted on purpose: pkg-config hands back one
    # space-separated string, and splitting it is the intended reading.
    if ! "$cxx" -std=c++17 -fsyntax-only -Wno-unused-command-line-argument \
        "${includes[@]}" $qt $ffmpeg \
        -DMOQ_FRONTEND_ENABLED -DMOQ_VERSION_STRING='"0.0.0"' "$source"; then
        status=1
    fi
done
exit $status
