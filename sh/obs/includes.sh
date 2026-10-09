#!/usr/bin/env bash
# Print the -I flags for libobs and the moq C++ package, shared by compile.sh
# and unit.sh.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$here/../../cpp/obs"

# The dev shell's pinned headers first, so every platform checks against the
# same libobs. Then the obs-deps framework (macOS/Windows), the OBS sources
# CMake unpacks beside it, and a system install (Linux without nix).
obs_include=""
for candidate in \
    "${OBS_INCLUDE_DIR:-}" \
    .deps/Frameworks/libobs.framework/Versions/A/Headers \
    .deps/obs-studio-*/libobs; do
    if [ -n "$candidate" ] && [ -f "$candidate/obs.h" ]; then
        obs_include="$candidate"
        break
    fi
done
if [ -z "$obs_include" ] && command -v pkg-config >/dev/null && pkg-config --exists libobs; then
    obs_include=$(pkg-config --variable=includedir libobs)/obs
fi
if [ -z "$obs_include" ]; then
    echo "libobs headers not found; run inside 'nix develop' (or set OBS_INCLUDE_DIR)" >&2
    exit 1
fi

# obs.h reaches <simde/x86/sse2.h> as of OBS 32, which used to be vendored
# under libobs/util/ and so came along with the headers above. The dev shell
# carries it (see flake.nix); the obs-deps bundle ships its own copy beside
# libobs, which is how the macOS and Windows builds get it.
simde_include=""
if command -v pkg-config >/dev/null && pkg-config --exists simde; then
    simde_include=$(pkg-config --variable=includedir simde)
else
    for candidate in .deps/obs-deps-*/include; do
        if [ -f "$candidate/simde/x86/sse2.h" ]; then
            simde_include="$candidate"
            break
        fi
    done
fi
if [ -z "$simde_include" ]; then
    echo "simde headers not found; run inside 'nix develop', which supplies them" >&2
    exit 1
fi

moq_prefix=$("$here/moq.sh")
moq_include="$moq_prefix/include"

# One flag per line, path glued to the -I, so a caller can rebuild the
# arguments without splitting on the spaces in a path.
printf -- '-I%s\n' src "$obs_include" "$simde_include" "$moq_include"
