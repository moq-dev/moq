# [M] Rust and JS container reads drop groups below the cursor

## Goal

The Rust and JS container consumers agree on groups that arrive below the read
cursor: neither delivers them. The read stays `Frame | Gap | End` in delivery
order past the cursor, and what lies below is already covered by the `Gap`
that moved the cursor. This is the ordered read only: APIs that yield in
arrival order keep every group. Startup adds no latency: the cursor starts
from the bounds the publisher reports, never from a wait.

## Plan

Found while landing #5288: JS (`js/hang/src/container/consumer.ts`) still
delivers a group that arrives below the cursor as a late backfill; Rust
(`rs/moq-mux/src/container/consumer.rs`) never does.

- ✅ Decided 2026-10-13: both drop, matching Rust. Not chosen: both deliver,
  flagged so readers can skip; leave it open.
- ✅ Decided 2026-10-15 (maintainer), superseding the 2026-10-13 settlement
  window: no window. Holding the first delivery for a max-delay span adds
  startup latency to every ordered read without a declared start, and
  [#5508](https://github.com/moq-dev/moq/pull/5508) (which implemented it) was
  closed. Current behavior stays until the publisher reports the track's
  bounds; then the ordered read takes its floor from them.
- Groups are served newest-first, so the head of a retained window can arrive
  after the live edge; a cursor set from the first arrival would lose valid
  startup media (`consumer.outoforder.test.ts` covers that order). With
  bounds known up front, the floor is the oldest group the subscription
  covers ([Subscribe ranges](/quest/m1/subscribe-ranges/README.md)) and the
  live edge is the reported largest location
  ([Live largest location](/quest/m1/largest.md)), so the read starts at the
  right group without waiting.
- Once both land: set the ordered read's floor from those bounds in Rust and
  JS, then make JS discard groups below the cursor. Add a test mirroring
  Rust's: a reordered group below the floor is never yielded and no extra
  `Gap` appears, and the out-of-order head case keeps passing.
- Check JS readers that relied on backfill (watch's audio/video decoders,
  `Sync`) and adjust them; one that wants every group reads an arrival-order
  API instead.

Public API: no type change; the ordered read changes in Rust and JS (the
floor comes from reported bounds, and JS drops backfill). Wire: none beyond
the required quests.

## Required

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - the subscription reports the oldest group it covers
- [Live largest location](/quest/m1/largest.md) - the reader learns the live edge before the first group arrives
