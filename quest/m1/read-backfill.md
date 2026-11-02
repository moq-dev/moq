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
  startup media (`consumer.outoforder.test.ts` covers that order). The floor
  is instead the lowest group of the subscription's requested ranges
  ([Subscribe ranges](/quest/m1/subscribe-ranges/README.md)): given for an
  absolute start, and for a relative start the start the publisher resolved
  for this request. Every sequence in range arrives or is named by
  SUBSCRIBE_DROP, so the read never waits on a group that won't come.
- Don't infer a relative start from the reported largest location: a
  frameless newest group counts when a relative end resolves but not in
  `largest()`, sparse moq-transport groups break the arithmetic, and a later
  coalesced largest isn't this request's resolution. Lite-07 SUBSCRIBE_OK's
  `Range Start` carries where each range's delivery starts, from which its
  groups are owed (`lite::SubscribeStart::starts`, 0 on the wire for a range
  with none); the Rust copy only takes the lowest today, so carry each to the
  reader. Settle the moq-transport equivalent or keep today's first-arrival
  cursor there.
- The cursor is range-aware, not just floored: with disjoint ranges it skips
  straight across an unrequested gap (group 1, then 10 onward) instead of
  waiting for the latency budget, since no SUBSCRIBE_DROP is owed there.
- Once both land: set the ordered read's floor from those bounds in Rust and
  JS, then make JS discard groups below the cursor. Add a test mirroring
  Rust's: a reordered group below the floor is never yielded and no extra
  `Gap` appears, and the out-of-order head case keeps passing. Add a
  disjoint live-range regression for the skip, and a relative start whose
  newest group has no frames yet.
- Check JS readers that relied on backfill (watch's audio/video decoders,
  `Sync`) and adjust them; one that wants every group reads an arrival-order
  API instead.

Decided 2026-10-27 (maintainer): this waits only on the resolved start in
SUBSCRIBE_OK, now landed, and JS ranges, not the whole
[Subscribe ranges](/quest/m1/subscribe-ranges/README.md) epic.

Public API: no type change; the ordered read changes in Rust and JS (the
floor comes from reported bounds, and JS drops backfill). Wire: none.

## Required

- [JS ranges](/quest/m1/subscribe-ranges/js.md) - the JS reader's requested ranges set its floor
