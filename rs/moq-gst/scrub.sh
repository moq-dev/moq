#!/usr/bin/env bash
#
# Rewrite shared-library paths in a moq-gst plugin so it loads against
# the user's system GStreamer instead of the nix copy it was linked
# against. Used by build.sh on the tarball / .deb / .rpm release path;
# `nix build .#moq-gst` does not run this, so the flake output stays
# usable inside `nix shell` / cachix.
#
# Usage:
#   scrub.sh <path-to-libgstmoq.so|.dylib>
#
# Dispatch is by extension. On macOS, every absolute LC_LOAD_DYLIB
# outside system locations is rewritten to @rpath/<basename>, every
# /nix LC_RPATH is stripped, and rpaths for the three usual GStreamer
# install prefixes plus /usr/lib (for libiconv via dyld_shared_cache)
# are added. On Linux, patchelf --remove-rpath lets ld.so.cache do the
# work.

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 <path-to-libgstmoq.{so,dylib}>" >&2
    exit 2
fi

lib="$1"
if [[ ! -f "$lib" ]]; then
    echo "Error: $lib does not exist" >&2
    exit 2
fi

scrub_macos() {
    local dylib="$1"

    # Rewrite every absolute LC_LOAD_DYLIB outside system dirs to
    # @rpath/<basename>. Allowlist (not "grep nix") so we also catch
    # any non-nix leakage we haven't predicted.
    # `tail -n +2` skips line 1, the dylib's own LC_ID_DYLIB.
    otool -L "$dylib" |
        tail -n +2 |
        awk '{print $1}' |
        { grep -vE '^(@|/usr/lib/|/System/)' || true; } |
        sort -u |
        while read -r ref; do
            install_name_tool -change "$ref" "@rpath/$(basename "$ref")" "$dylib"
        done

    # Strip LC_RPATH entries under /nix.
    otool -l "$dylib" |
        awk '/^ *cmd LC_RPATH$/{f=1; next} f && /^ *path /{print $2; f=0}' |
        { grep '^/nix/' || true; } |
        while read -r rp; do
            install_name_tool -delete_rpath "$rp" "$dylib"
        done

    # /opt/homebrew + /usr/local cover homebrew on ARM and Intel; the
    # Framework path covers the official .pkg installer; /usr/lib lets
    # dyld resolve system libs (libiconv, libc++) via dyld_shared_cache
    # at @rpath substitution time.
    install_name_tool -add_rpath /opt/homebrew/lib "$dylib"
    install_name_tool -add_rpath /usr/local/lib "$dylib"
    install_name_tool -add_rpath /Library/Frameworks/GStreamer.framework/Libraries "$dylib"
    install_name_tool -add_rpath /usr/lib "$dylib"

    # Whitelist assertion: every LC_LOAD_DYLIB must resolve via @rpath,
    # @loader_path, @executable_path, /usr/lib, or /System.
    local bad
    bad="$(otool -L "$dylib" |
        tail -n +2 |
        awk '{print $1}' |
        { grep -vE '^(@|/usr/lib/|/System/)' || true; })"
    if [[ -n "$bad" ]]; then
        echo "Error: $dylib has non-portable LC_LOAD_DYLIB entries:" >&2
        echo "$bad" >&2
        exit 1
    fi
    local bad_rp
    bad_rp="$(otool -l "$dylib" |
        awk '/^ *cmd LC_RPATH$/{f=1; next} f && /^ *path /{print $2; f=0}' |
        { grep '^/nix/' || true; })"
    if [[ -n "$bad_rp" ]]; then
        echo "Error: $dylib has /nix LC_RPATH entries:" >&2
        echo "$bad_rp" >&2
        exit 1
    fi
}

scrub_linux() {
    local so="$1"

    patchelf --remove-rpath "$so"

    local rp
    rp="$(patchelf --print-rpath "$so")"
    if [[ -n "$rp" ]]; then
        echo "Error: $so still has DT_RPATH/DT_RUNPATH after strip: $rp" >&2
        exit 1
    fi
    if patchelf --print-needed "$so" | grep -q '/nix/'; then
        echo "Error: $so has DT_NEEDED entries with /nix paths:" >&2
        patchelf --print-needed "$so" | grep '/nix/' >&2
        exit 1
    fi
}

case "$lib" in
    *.dylib) scrub_macos "$lib" ;;
    *.so) scrub_linux "$lib" ;;
    *)
        echo "Error: unrecognized extension on $lib (expected .so or .dylib)" >&2
        exit 2
        ;;
esac

echo "Scrubbed $lib for system GStreamer."
