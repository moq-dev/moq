# [L] A frame's bytes return their credit as they are read, not at its seal

## Goal

About 200 concurrent groups, each writing 64 KiB frames over one connection,
arrive losslessly when every reader is slower than its writer. Today they
evict with no paused stream refused, at the default 4 MiB connection window.

A stream mid-frame holds its unsealed bytes' connection credit until the
segment seals, so N such streams in flight at once need N frames of
connection window, less what paused streams hold. [#9210](https://github.com/moq-dev/moq/pull/9210)
sized a paused stream's allowance so any number pause, and the sim at 64 KiB
frames now meets this limit first: about 150 groups pass and 200 evict. The
limit holds for streams that have not paused yet, so no allowance split lifts
it.

## Plan

Possible direction, not decided: return a stream's credit as its bytes are
read, instead of at the seal. The obstacle is that a segment's bytes are
allocated against the origin's budget until it seals (`rs/moq-net/src/model`),
and the lease is released per segment
(`rs/moq-quic/src/connection/streams/lease.rs`), so crediting earlier means
either the budget charges a segment as it fills or the transport holds
credit for bytes the model already counts. It is a design change of its own,
as [unbounded groups](/quest/m3/unbounded-groups/README.md) made the release
per segment.

Reproduce with `quic_sim::many_groups_carry_bulk_transfers_losslessly` at 200
groups instead of `MANY`. The fix raises `MANY` until a frame's worth of
credit per stream no longer bounds it, or says what bounds it instead.

Whatever lands updates the credit model (the [`verify-credit` skill](/.claude/skills/verify-credit/SKILL.md)): early
return changes when credit comes back, and its safety property (credit held
together never exceeds the window) must still hold.

Public API: none expected. Wire: none.

## Related

- [`verify-credit` skill](/.claude/skills/verify-credit/SKILL.md) - models the credit this changes
- [Unbounded groups](/quest/m3/unbounded-groups/README.md) - decided the per-segment release
