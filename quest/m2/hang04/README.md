# hang-04 freezes a catalog worth keeping

## Goal

The catalog JSON that the cut publishes as moq-hang-04 is the one a later
version can grow from: the broadcast's clock and join interval sit in a
section named for what they are, a player skips an entry it cannot read
instead of losing the whole catalog, nothing is promised that no producer
writes, and a mis-shaped section means one thing. Each child changes that
JSON, in `rs/hang`, `js/hang`, and `drafts/draft-lcurley-moq-hang.md`, while
hang-04 is still unpublished and every change is free.

This README's own work is the last read before the freeze: once the children
have landed, the draft's `moq-hang-04` changelog describes the catalog that
ships (not the steps main took to reach it), `js/hang`'s
`catalog/parity.test.ts` and the Rust fixtures cover every section the draft
defines, and `just drafts check` passes.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit). The audit
read the catalog as a frozen format: hang-03 is what the datatracker and
`release` carry, so every hang-04 change on main is still open, and the 0.22
readers are the first that check `version` at all. Each child's Plan records
its own decisions.

The children land in any order.

Every root section already reads under the hang draft's one rule (a root
member the draft defines has its shape, or the catalog is refused), so
the `sync` section and
[the MPEG-TS section](/quest/m2/hang04/m2ts.md) are born strict.

What the audit judged and kept, so nobody reopens it without new evidence:
`include`, `track` apart from the ID, `instance` on references, `enabled`,
`preference`, `reorder`, `color`, `projection`, `stereo`, audio `layout`,
`interlaced`, audio `warmup`, `jitter`, the data tracks' estimates, `stats`,
and `echo`.

The hang draft's duration and discontinuity marker rules are rewritten by
[media consumers](/quest/m2/time-addressing/consumers.md), which now lands
before the cut, so hang-04 never specifies the markers.

Public API: each child's. Wire: the hang-04 catalog JSON, unpublished.

## Related

- [Media consumers address groups by start](/quest/m2/time-addressing/consumers.md) - removes the markers from the draft's container rules
