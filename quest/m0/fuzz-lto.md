# [XS] Fuzz targets build under the release profile's LTO

## Goal

Every nightly `Fuzz (<target>)` job builds and runs its target again. A
release-profile change can't silently break the fuzz build, because the
fuzz build opts out of what sanitizer coverage can't link.

## Plan

Seen in the 2026-10-12 nightly (run 38276133321): all six fuzz targets fail
to link with `undefined reference to '__sancov_gen_.N'` from
`asan.module_dtor`. The 2026-10-11 nightly passed.

Cause: #5274 (be8d0024d) set `lto = "fat"`, `codegen-units = 1` and
`strip = "symbols"` on the workspace `[profile.release]`. `cargo fuzz`
builds `--release`, and `rs/moq-net/fuzz` is a workspace member, so it
inherits fat LTO, which drops the sanitizer-coverage globals the ASan module
destructor references. Reproduced locally on the dev shell's rustc with
`RUSTC_BOOTSTRAP=1` and cargo-fuzz's flags (`cargo build --manifest-path
rs/moq-net/fuzz/Cargo.toml --release --bin lite`): fails as in CI, and links
with `CARGO_PROFILE_RELEASE_LTO=false`.

Likely a one-line fix: `sh/rs/fuzz.sh` already exports `RUSTFLAGS`, so it can
override the profile the same way (for example
`CARGO_PROFILE_RELEASE_LTO=false`). Consider `strip = "none"` too, so ASan
and libFuzzer crash reports keep symbol names. Prefer whatever keeps the
fuzz build's settings next to the fuzz build rather than another profile in
the root `Cargo.toml`, unless cargo-fuzz can select one cleanly.

Verify with `just rs fuzz <target> -- -max_total_time=10` for each target.
The nightly is the only CI that builds fuzz targets, so the PR can't prove
it on CI; say so in the PR.

Public API: none. Wire: none.

## Related

- [Release builds link again](/quest/m0/release-link.md) - the other build break in the same nightly, from a different cause
