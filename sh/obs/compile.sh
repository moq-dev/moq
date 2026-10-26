#!/usr/bin/env bash
# Type-check every plugin source and unit test, without linking the plugin or an
# obs-deps download.
#
# This is the gate to run in a worktree. `just obs build` needs the
# multi-hundred-MB obs-deps bundle (macOS/Windows); this doesn't, because the dev
# shell carries the OBS-side header sets: libobs pinned to the same OBS release
# buildspec.json downloads (see `obs-headers` in flake.nix), Qt6, and ffmpeg. The
# moq headers are generated, so it builds rs/moq-ffi to render them. CI links the
# real thing on Linux (.github/workflows/obs.yml).
#
# It compiles the Qt sources too, which the CMake build only does when
# ENABLE_QT and ENABLE_FRONTEND_API are on, so the definitions they gate on are
# set here to match.
#
# test/ is in scope because the tests drive the plugin through the same
# generated moq headers, so an API that drifts under them fails here rather than
# only where obs.yml's path filter reaches; `just obs test` runs nightly.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

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

# Sources check in parallel under the same bound as the module lanes
# sh/dispatch.sh runs at once, and with the same default.
limit=${CHECK_JOBS:-3}
if [[ ! "$limit" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: CHECK_JOBS must be a positive integer, not '$limit'" >&2
    exit 2
fi

# Assign first so a failure in includes.sh still aborts under `set -e`,
# then split on newlines only: an include path may contain spaces.
includes_raw=$("$here/includes.sh")
includes=()
while IFS= read -r flag; do includes+=("$flag"); done <<<"$includes_raw"

qt=$(pkg-config --cflags Qt6Widgets Qt6Gui Qt6Core)
ffmpeg=$(pkg-config --cflags libavcodec libavutil libswscale libswresample)

logs=$(mktemp -d)
declare -A running=()
# Each check runs in its own process group, so a cancel reaches the compiler
# proper that the driver forks, not only the driver.
trap 'for pid in "${!running[@]}"; do kill -TERM -- "-$pid" 2>/dev/null || true; done; wait; rm -rf "$logs"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# A failing source does not stop the rest, so one run reports every broken
# file. Each source's diagnostics print whole once it finishes, rather than
# interleaved with the checks beside it.
sources=(src/*.cpp test/*.cpp)
next=0
status=0
while true; do
    while ((next < ${#sources[@]} && ${#running[@]} < limit)); do
        set -m
        # shellcheck disable=SC2086
        # MOQ_VERSION_STRING only reaches a label in the dock, so any value
        # type-checks the same; CMake stamps the real moq-cpp version.
        # -Werror: a warning that only prints is never fixed.
        # -Wno-unused-command-line-argument: the nix compiler wrapper injects
        # link flags, which a syntax-only run reports as unused, once per file.
        # $qt and $ffmpeg stay unquoted on purpose: pkg-config hands back one
        # space-separated string, and splitting it is the intended reading.
        # The job is a subshell around the compiler, so it exits even when the
        # compiler is killed: bash 5.2 drops a job that a signal ends before
        # `wait -n` collects it. The trailing exit keeps bash from exec'ing the
        # compiler in the subshell's place.
        (
            "$cxx" -std=c++17 -fsyntax-only -Werror -Wno-unused-command-line-argument \
                "${includes[@]}" $qt $ffmpeg \
                -DMOQ_FRONTEND_ENABLED -DMOQ_VERSION_STRING='"0.0.0"' "${sources[next]}"
            exit
        ) </dev/null >"$logs/$next" 2>&1 &
        set +m
        running[$!]=$next
        next=$((next + 1))
    done
    ((${#running[@]})) || break

    result=0
    wait -n -p pid "${!running[@]}" || result=$?
    index=${running[$pid]}
    unset "running[$pid]"
    cat "$logs/$index" >&2
    # Named here because a compiler killed outright, such as by the OOM killer
    # under concurrent checks, prints nothing that says which source it was.
    if ((result)); then
        echo "error: ${sources[index]} failed with status $result" >&2
        status=1
    fi
done
exit $status
