# Sourced: put a rustup nightly's cargo and rustc first on PATH, for the Nightly lanes
# that need unstable flags (Miri, ThreadSanitizer). COMPONENTS names what the caller
# needs from it, for the hint when it is missing.
if [[ "$(rustc --version)" == *-nightly* ]]; then
    sysroot=$(rustc --print sysroot)
elif command -v rustup >/dev/null 2>&1; then
    sysroot=$(rustup run nightly rustc --print sysroot)
else
    echo "rs: nightly Rust is missing; select it or run rustup toolchain install nightly --component ${COMPONENTS:?}" >&2
    exit 1
fi

# Nix puts its pinned stable compiler first; cargo and rustc must both be nightly.
export PATH="$sysroot/bin:$PATH"
rustc --version
