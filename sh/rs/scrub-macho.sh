#!/usr/bin/env bash
#
# Rewrite a macOS Mach-O file (executable or .dylib) so it loads against
# the system / user-installed libraries instead of the nix-store copies it
# was linked against. The nix toolchain links its own libiconv (and
# friends) by absolute /nix/store path, which doesn't exist on a user's
# Mac, so dyld aborts at startup.
#
# Every absolute LC_LOAD_DYLIB outside /usr/lib and /System is rewritten to
# @rpath/<basename>, every /nix LC_RPATH is stripped, and the rpaths passed
# as arguments are added so dyld can find the libraries. /usr/lib lets dyld
# resolve system libs (libiconv, libc++) via the dyld_shared_cache at
# @rpath substitution time. Asserts nothing resolves through /nix afterward.
#
# Usage:
#   scrub-macho.sh <macho-file> [rpath ...]
#
# With no rpaths, defaults to /usr/lib (enough for a plain binary whose only
# leak is the system-cache libiconv). Callers that ship into a plugin host
# (e.g. moq-gst) pass their install prefixes too.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <macho-file> [rpath ...]" >&2
    exit 2
fi

macho="$1"
shift
rpaths=("$@")
if [[ ${#rpaths[@]} -eq 0 ]]; then
    rpaths=(/usr/lib)
fi

if [[ ! -f "$macho" ]]; then
    echo "Error: $macho does not exist" >&2
    exit 2
fi

# Rewrite every absolute LC_LOAD_DYLIB outside system dirs to
# @rpath/<basename>. Allowlist (not "grep nix") so we also catch any
# non-nix leakage we haven't predicted. `tail -n +2` skips line 1: a
# dylib's own LC_ID_DYLIB, or an executable's "<path>:" header.
otool -L "$macho" |
    tail -n +2 |
    awk '{print $1}' |
    { grep -vE '^(@|/usr/lib/|/System/)' || true; } |
    sort -u |
    while read -r ref; do
        install_name_tool -change "$ref" "@rpath/$(basename "$ref")" "$macho"
    done

# Strip LC_RPATH entries under /nix.
otool -l "$macho" |
    awk '/^ *cmd LC_RPATH$/{f=1; next} f && /^ *path /{print $2; f=0}' |
    { grep '^/nix/' || true; } |
    while read -r rp; do
        install_name_tool -delete_rpath "$rp" "$macho"
    done

# install_name_tool -add_rpath errors (and aborts the script under set -e)
# if the rpath is already present, so skip ones that already exist.
existing_rpaths="$(otool -l "$macho" |
    awk '/^ *cmd LC_RPATH$/{f=1; next} f && /^ *path /{print $2; f=0}')"
for rp in "${rpaths[@]}"; do
    if ! grep -Fxq "$rp" <<<"$existing_rpaths"; then
        install_name_tool -add_rpath "$rp" "$macho"
    fi
done

# Whitelist assertion: every LC_LOAD_DYLIB must resolve via @rpath,
# @loader_path, @executable_path, /usr/lib, or /System.
bad="$(otool -L "$macho" |
    tail -n +2 |
    awk '{print $1}' |
    { grep -vE '^(@|/usr/lib/|/System/)' || true; })"
if [[ -n "$bad" ]]; then
    echo "Error: $macho has non-portable LC_LOAD_DYLIB entries:" >&2
    echo "$bad" >&2
    exit 1
fi

bad_rp="$(otool -l "$macho" |
    awk '/^ *cmd LC_RPATH$/{f=1; next} f && /^ *path /{print $2; f=0}' |
    { grep '^/nix/' || true; })"
if [[ -n "$bad_rp" ]]; then
    echo "Error: $macho has /nix LC_RPATH entries:" >&2
    echo "$bad_rp" >&2
    exit 1
fi
