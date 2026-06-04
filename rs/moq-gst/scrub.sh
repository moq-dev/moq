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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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

    # The shared Mach-O scrub does the LC_LOAD_DYLIB rewrite, /nix LC_RPATH
    # strip, and the no-/nix assertion. We just pass the rpaths a GStreamer
    # plugin needs: /opt/homebrew + /usr/local cover homebrew on ARM and
    # Intel, the Framework path covers the official .pkg installer, and
    # /usr/lib lets dyld resolve system libs (libiconv, libc++) via the
    # dyld_shared_cache at @rpath substitution time.
    "$SCRIPT_DIR/../scripts/scrub-macho.sh" "$dylib" \
        /opt/homebrew/lib \
        /usr/local/lib \
        /Library/Frameworks/GStreamer.framework/Libraries \
        /usr/lib
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
