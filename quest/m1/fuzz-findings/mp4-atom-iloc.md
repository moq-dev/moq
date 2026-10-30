# [S] mp4-atom iloc is bounded

## Goal

An `iloc` box costs mp4-atom heap proportional to its size: in the `cmaf`
target, 31 bytes of `iloc` no longer allocate about 6 MB.

## Plan

`src/meta/iloc.rs` decodes `extent_count` extents per item. When the
offset, length and index sizes are all 0, each extent consumes no input, so
every 6-byte item can push 65,535 extents (about 1.5 MB), and the item count
repeats that.

Fix it in [kixelated/mp4-atom](https://github.com/kixelated/mp4-atom), as
[#264](https://github.com/kixelated/mp4-atom/pull/264) did for `sgpd`. A
plain count-versus-remaining-bytes check, which `sgpd` and `evte` use, would
refuse a valid single zero-size extent at the end of the box. Open, for the
maintainer before building, with a recommendation: reject zero-size extents
beyond the first in an item, store a repeated zero-size extent as a count,
or bound the total extents by the box size plus the item count.

Then bump moq-mux with the
[mp4-atom bump](/quest/m1/fuzz-findings/mp4-atom-bump.md), or its own bump if
that has landed, and commit the input under `rs/moq-mux/fuzz/regressions/`.

Public API: none expected in moq-mux. Wire: none.
