# [S] Priority reorder without a channel write per shifted entry

## Goal

The publisher-side `PriorityQueue` is one mutex per session, and inserting a
group into a busy queue calls `update_indices_from` on every entry after the
insertion point. Each of those does a full `kio::Producer<u8>` write, a lock
acquisition plus a waiter drain and wake, for up to `MAX_VEC_SIZE` (255)
entries. The overflow heap's `extract` is an O(n) drain-and-rebuild. A
single insert should cost one lock and at most one wake.

## Plan

- Decouple send order from materialized vec indices: either compare ranks
  at pop time (the consumer asks "who is first now" instead of being told
  its index changed), or coalesce the reorder into one generation bump that
  wakes the session driver once and lets it re-read order lazily.
- Reshape overflow extraction so promoting from the heap is O(log n).
- The wire semantics are fixed and must not change: lite rank and the IETF
  wire are lower-first, and the existing ordering tests plus the send-order
  integration coverage must pass unmodified.
- Three mutex trips per served group (insert, drop, update) can likely
  become two or fewer once the index-update channel writes are gone;
  measure rather than chase.

Acceptance: a micro-bench of insert cost at queue depths 8, 64, and 255,
plus groups per second on a session with many concurrent subscriptions.
Relay bench must not regress.
