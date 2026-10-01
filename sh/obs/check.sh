#!/usr/bin/env bash
# Lint formatting. Skips a tool silently if it isn't on $PATH (matches repo convention).
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../cpp/obs"

if grep -Fq '"MOQ_VERSION"' CMakePresets.json; then
    echo "CMakePresets.json must not pin MOQ_VERSION" >&2
    exit 1
fi
# The headers `just obs compile` type-checks against are a second copy of
# the OBS version this spec downloads, so a bump here has to reach flake.nix
# or the two gates check different libobs.
spec_obs=$(jq -r '.dependencies["obs-studio"].version // ""' buildspec.json)
flake_obs=$(sed -n '/pname = "libobs-headers"/,/}/p' ../../flake.nix | sed -n 's/.*version = "\([^"]*\)".*/\1/p')
# Both are pattern matches against files this recipe doesn't own the shape
# of. An empty parse compares equal to an empty parse, so the guard would
# pass by reporting nothing rather than by finding the versions in sync.
if [ -z "$spec_obs" ] || [ -z "$flake_obs" ]; then
    echo "couldn't read the obs-studio version: '$spec_obs' from buildspec.json, '$flake_obs' from flake.nix" >&2
    exit 1
fi
if [ "$spec_obs" != "$flake_obs" ]; then
    echo "obs-studio is $spec_obs in buildspec.json but $flake_obs in flake.nix (libobs-headers)" >&2
    exit 1
fi
# And a third: `just obs ci` links against nixpkgs' obs-studio, the one OBS
# this repo doesn't pick. Without this the released macOS/Windows binaries
# could be a whole libobs API apart from what CI ever compiles. Patch
# releases carry no API change, so only the major.minor has to agree; that
# also keeps the guard quiet until flake.lock actually moves OBS, which is
# the change that opens the gap and so the one that should fail.
# OBS_LINKED_VERSION comes from the dev shell (see flake.nix), so this leg
# is skipped outside it, where there is no nixpkgs obs-studio to compare.
# MOQ_STRICT turns that skip into an error for the same reason it turns a
# missing tool into one: in CI a leg that checks nothing still reports green.
if [ -z "${OBS_LINKED_VERSION:-}" ]; then
    if [ -n "${MOQ_STRICT:-}" ]; then
        echo "MOQ_STRICT is set but OBS_LINKED_VERSION is unset; run inside 'nix develop', which exports it" >&2
        exit 1
    fi
else
    spec_api=$(echo "$spec_obs" | cut -d. -f1,2)
    linked_api=$(echo "$OBS_LINKED_VERSION" | cut -d. -f1,2)
    if [ "$spec_api" != "$linked_api" ]; then
        echo "obs-studio is $spec_obs in buildspec.json but $OBS_LINKED_VERSION in nixpkgs, which is what 'just obs ci' links" >&2
        echo "bump buildspec.json and flake.nix's libobs-headers to $OBS_LINKED_VERSION, or pin nixpkgs back" >&2
        exit 1
    fi
fi
if command -v cmake >/dev/null; then
    build_dir=$(mktemp -d)
    trap 'rm -rf "$build_dir"' EXIT
    if cmake -S . -B "$build_dir" -DMOQ_LOCAL= >"$build_dir/configure.log" 2>&1; then
        echo "CMake accepted a release build without MOQ_VERSION" >&2
        exit 1
    fi
    if ! grep -Fq "MOQ_VERSION is required when MOQ_LOCAL does not contain rs/libmoq" "$build_dir/configure.log"; then
        cat "$build_dir/configure.log" >&2
        exit 1
    fi
fi
if command -v clang-format >/dev/null; then
    git ls-files 'src/*.cpp' 'src/*.h' 'test/*.cpp' | xargs clang-format --dry-run --Werror
fi
if command -v gersemi >/dev/null; then
    gersemi --check CMakeLists.txt cmake
fi
