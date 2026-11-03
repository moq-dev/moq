# [XS] mp4-atom iloc is bounded

## Goal

An `iloc` box costs mp4-atom heap proportional to its size: in the `cmaf`
target, 31 bytes of `iloc` no longer allocate about 6 MB.

## Plan

`src/meta/iloc.rs` decodes `extent_count` extents per item. When the
offset, length and index sizes are all 0, each extent consumes no input, so
every 6-byte item can push 65,535 extents (about 1.5 MB), and the item count
repeats that.

[kixelated/mp4-atom#267](https://github.com/kixelated/mp4-atom/pull/267)
fixes it, as [#264](https://github.com/kixelated/mp4-atom/pull/264) did for
`sgpd`: decode fails with `OutOfBounds` on a zero-size extent (one that takes
no bytes of the box) after an item's first extent, so a valid single
zero-size extent still decodes (decided 2026-10-30, maintainer). A plain
count-versus-remaining-bytes check, which `sgpd` and `evte` use, would refuse
that valid extent at the end of the box. The fix does not cut a release.

Once #267 merges, moq-mux picks it up with the
[mp4-atom bump](/quest/m1/fuzz-findings/mp4-atom-bump.md), or with its own
bump if that one has already landed. Then commit the 31-byte `cmaf` input from
[moq-dev/moq#7313's comment](https://github.com/moq-dev/moq/pull/7313#issuecomment-6520535160)
(`AB0AAAAcaWxvYwAAcwAAACAAyP////8AAAD///8AAA==`) as
`rs/moq-mux/fuzz/regressions/cmaf/mp4-atom-iloc`. Do not commit it before the
bump, since the merge gate would fail on it.

Public API: none expected in moq-mux. Wire: none.

## Waiting

- Maintainer: merge [kixelated/mp4-atom#267](https://github.com/kixelated/mp4-atom/pull/267); check that it is merged and that the mp4-atom release the bump moves to contains its commit
