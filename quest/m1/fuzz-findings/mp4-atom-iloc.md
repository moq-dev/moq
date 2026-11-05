# [XS] mp4-atom iloc is bounded

## Goal

An `iloc` box costs mp4-atom heap proportional to its size: in the `cmaf`
target, 31 bytes of `iloc` no longer allocate about 6 MB.

## Plan

`src/meta/iloc.rs` decoded `extent_count` extents per item. When the
offset, length and index sizes are all 0, each extent consumes no input, so
every 6-byte item could push 65,535 extents (about 1.5 MB), and the item
count repeated that.

[kixelated/mp4-atom#267](https://github.com/kixelated/mp4-atom/pull/267)
fixed it, merging on 2026-11-03 as
kixelated/mp4-atom@2f1936206579b4c64d1d90538989246044e42ed6: decode fails
with `OutOfBounds` on a zero-size extent after an item's first extent, so a
valid single zero-size extent still decodes (decided 2026-10-30,
maintainer). The pending release PR
[kixelated/mp4-atom#260](https://github.com/kixelated/mp4-atom/pull/260)
picks it up.

What remains is moq-mux's move to that release and the 31-byte `cmaf`
regression input, both tracked in the
[mp4-atom bump](/quest/m1/fuzz-findings/mp4-atom-bump.md); its PR completes
this quest and deletes this file.

Public API: none expected in moq-mux. Wire: none.

## Waiting

- Maintainer: cut an mp4-atom 0.17.x release that contains kixelated/mp4-atom@2f1936206579b4c64d1d90538989246044e42ed6 (agents do not cut it); check that crates.io's latest mp4-atom is above 0.17.0 and that its tag contains that commit
