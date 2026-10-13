# [S] Release builds link again on the current Nix toolchain

## Goal

The Linux release binaries (`moq-cli`, `moq-relay`) and the `moq-gst`
.deb/.rpm packages build with `cargo zigbuild` on `main`'s pinned Nix inputs,
for x86_64 and aarch64, so the next release cut from `main` can publish.

## Plan

Seen in the 2026-10-12 nightly (run 38276133321): `moq-cli / release /
Build`, `moq-relay / release / Build` and `moq-gst / Package .deb/.rpm`
fail on both Linux targets. Linking `iroh-relay`'s cdylib fails with
`ld.lld: error: undefined symbol:` (an empty name), referenced by
`bcm.o:(aws_lc_0_45_0_OPENSSL_cpuid_setup)` in `aws-lc-sys`. The 2026-10-11
nightly passed. macOS and Windows release builds still pass.

Onset: the nixpkgs bump in #5316 (d0736ec04), not #5274's LTO change, which
landed the same night. Reproduced with
`cargo zigbuild --locked --release --target x86_64-unknown-linux-gnu.2.34 -p iroh-relay --lib`:

- Current `flake.lock`: fails as in CI, also with
  `CARGO_PROFILE_RELEASE_LTO=false` and with `CARGO_PROFILE_RELEASE_STRIP=none`.
- `flake.lock` from 717f3fecc (before #5316), same `Cargo.toml` and
  `Cargo.lock`: links.
- Both shells have zig 0.16.0 and rustc 1.98.1. What changed is GCC
  15.3 -> 16.2, glibc 2.42 -> 2.44, and smaller bumps.
- `bcm.o` in the new build has 292 undefined `SECTION` symbols in
  `readelf -s`; the old one has none. Both report clang 21.1.8 in `.comment`,
  so something other than the compiler zig picks differs, possibly a step in
  `aws-lc-sys`'s build that reaches the host toolchain.

Find which tool produces the broken object and fix it at the source: an
`aws-lc-sys` or cargo-zigbuild environment fix, or an upstream report if the
bug is theirs. Reverting nixpkgs is a stopgap, not the fix; if it's needed
to unblock a release, make it its own quest.

`release` still pins the pre-bump nixpkgs, so shipping releases are fine
today; the next `main` -> `release` merge carries the break. PR CI never runs
zigbuild, so nothing caught it before the nightly. Consider a cheap PR-time
check when `flake.lock` changes, if the fix shows one is worth it.

Public API: none. Wire: none.

## Related

- [Fuzz targets build under LTO](/quest/m0/fuzz-lto.md) - the other build break in the same nightly, from a different cause
