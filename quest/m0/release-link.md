# [XS] Release builds link again on the current Nix toolchain

## Goal

The Linux release binaries (`moq-cli`, `moq-relay`) and the `moq-gst`
.deb/.rpm packages build with `cargo zigbuild` on `main`'s pinned Nix inputs,
for x86_64 and aarch64, so the next release cut from `main` can publish.

## Plan

Seen in the 2026-10-12 nightly (run 38276133321): `moq-cli / release /
Build`, `moq-relay / release / Build` and `moq-gst / Package .deb/.rpm`
fail on both Linux targets with `ld.lld: error: undefined symbol:` (an empty
name), referenced by `bcm.o:(aws_lc_0_45_0_OPENSSL_cpuid_setup)` in
`aws-lc-sys`. The nixpkgs bump in #5316 (d0736ec04) caused it.

Root cause (found 2026-10-13): nixpkgs' lld 21.1.8 built with GCC 16 is
miscompiled. lld leaves `Symbol` fields uninitialized behind a memset GCC 16
may drop, so a relocatable link (`ld.lld -r`) writes unnamed `STT_SECTION`
symbols with `SHN_UNDEF`, a different count each run. `zig cc -c` runs
`zig ld.lld -r` on every object it compiles, and zig links lld statically,
so zig's `bcm.o` carries the bad symbols. clang's own `bcm.o` is clean.
`ld.lld -r -o out.o bcm.o` on that clean object reproduces it with the
current nixpkgs lld 21 and not with the pre-bump one, lld 22, or
`staging-next`'s lld 21. Tracked upstream as NixOS/nixpkgs#571232; fixed by
NixOS/nixpkgs#568680. With `staging-next`'s (aaba2f7c9) `zig` and
`cargo-zigbuild`, `cargo zigbuild --locked --release --target
x86_64-unknown-linux-gnu.2.34 -p moq-relay` links.

NixOS/nixpkgs#568680 is now in `nixpkgs-unstable`. Remaining work: bump
`flake.lock` (or take the weekly Dependabot nix bump), then confirm the same
command for `moq-relay`, `moq-cli` and `moq-gst` on both Linux targets.

`release` still pins the pre-bump nixpkgs, so shipping releases are fine;
only the next `main` -> `release` merge carries the break.

Decisions (maintainer, 2026-10-13):

- ✅ No stopgap pin of `zig` or `cargo-zigbuild` to the pre-bump nixpkgs.
  `release` is unaffected and the fix is already in `staging-next`. Revisit
  only if a release from `main` must ship before the fix lands.
- ✅ No PR-time zigbuild check on `flake.lock` changes. The nightly caught
  this within a day, and a full zigbuild per nix bump works against the CI
  cost plan (#5426).

Public API: none. Wire: none.
