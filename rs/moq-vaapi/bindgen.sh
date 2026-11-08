#!/usr/bin/env bash
# Regenerates src/bindings/libva.rs from the vendored headers in libva/. Needs
# bindgen-cli on the PATH (`nix shell nixpkgs#rust-bindgen --command ./bindgen.sh`).
# Passing a libva release tag (e.g. `./bindgen.sh 2.23.0`) first refreshes
# libva/ and LICENSE.libva from that release.
#
# The C types the bindings use are fixed-width or symbolic (`c_char`), so one
# file serves every target. The exception is libva's own: two VPP structs pad to
# a constant size with a reserved array whose length depends on the pointer
# width, so both layouts are generated and the lines that differ go under a cfg.
set -euo pipefail
cd "$(dirname "$0")"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ $# -gt 0 ]; then
    # Only what bindgen reads: meson.build (version), va/*.h{,.in}, va/drm/va_drm.h.
    git clone --depth 1 --branch "$1" https://github.com/intel/libva.git "$work/libva"
    rm -rf libva
    mkdir -p libva/va/drm
    cp "$work/libva/meson.build" libva/meson.build
    cp "$work"/libva/va/*.h "$work"/libva/va/*.h.in libva/va/
    cp "$work/libva/va/drm/va_drm.h" libva/va/drm/va_drm.h
    cp "$work/libva/COPYING" LICENSE.libva
fi

# libva generates va_version.h from a template at build time, and installs
# va_drm.h beside va.h; stage both where `#include <va/...>` finds them.
mkdir -p "$work/va"
version() { sed -nE "s/^$1\s*=\s*([0-9]+).*/\1/p" libva/meson.build; }
major=$(version va_api_major_version)
minor=$(version va_api_minor_version)
micro=$(version va_api_micro_version)
sed -e "s/@VA_API_MAJOR_VERSION@/$major/" -e "s/@VA_API_MINOR_VERSION@/$minor/" \
    -e "s/@VA_API_MICRO_VERSION@/$micro/" -e "s/@VA_API_VERSION@/$major.$minor.$micro/" \
    libva/va/va_version.h.in >"$work/va/va_version.h"
cp libva/va/drm/va_drm.h "$work/va/va_drm.h"

# `--dynamic-loading` emits a `Va` struct of function pointers that libloading
# resolves at runtime, rather than link-time externs, so libva is dlopen'd. A
# symbol a system libva older than these headers lacks errors only when called,
# not when the library loads. Freestanding, so the host's C library cannot
# change the output.
generate() {
    bindgen libva-wrapper.h \
        --rust-target 1.85 \
        --rust-edition 2021 \
        --no-layout-tests \
        --with-derive-default \
        --with-derive-eq \
        --constified-enum-module 'VA.*' \
        --allowlist-var 'VA.*' \
        --allowlist-function 'va.*' \
        --allowlist-type '.*ExternalBuffers.*|.*PRIME.*|.*H264.*|.*HEVC.*|VACodedBufferSegment|VAEncMisc.*|VASurfaceDecodeMBErrors|VADecodeErrorType|.*VAProc.*|VAEncPacked.*' \
        --dynamic-loading Va \
        --output "$2" \
        -- -Ilibva -I"$work" -ffreestanding --target="$1"
}
generate x86_64-unknown-linux-gnu "$work/wide.rs"
generate i686-unknown-linux-gnu "$work/narrow.rs"

# Only the reserved padding may differ; anything else needs a look before it is
# papered over with a cfg.
unexpected=$(diff "$work/wide.rs" "$work/narrow.rs" | grep -E '^[<>]' | grep -vE '^[<>]\s*pub va_reserved: \[u(16|32); [0-9]+usize\],$' || true)
if [ -n "$unexpected" ]; then
    printf 'the bindings differ by pointer width in more than libva'"'"'s reserved padding:\n%s\n' "$unexpected" >&2
    exit 1
fi
# The condition libva's headers pad on: x86_64, or LP64.
wide='any(target_arch = "x86_64", all(target_pointer_width = "64", not(windows)))'
diff --unchanged-line-format='%L' \
    --old-line-format=$'\t#[cfg('"$wide"$')]\n%L' \
    --new-line-format=$'\t#[cfg(not('"$wide"$'))]\n%L' \
    "$work/wide.rs" "$work/narrow.rs" >"$work/merged.rs" || [ $? -eq 1 ]
# bindgen bounds `Va::new` by `AsRef<OsStr>`, which libloading 0.9's
# `Library::new` no longer accepts.
grep -qF 'P: AsRef<::std::ffi::OsStr>,' "$work/merged.rs"
sed 's/P: AsRef<::std::ffi::OsStr>,$/P: ::libloading::AsFilename,/' "$work/merged.rs" >src/bindings/libva.rs
