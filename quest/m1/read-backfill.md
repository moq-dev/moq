# [S] Rust and JS container reads drop groups below the cursor

## Goal

The Rust and JS container consumers agree on groups that arrive below the read
cursor: neither delivers them. The read stays `Frame | Gap | End` in delivery
order past the cursor, and what lies below is already covered by the `Gap`
that moved the cursor.

## Plan

Found while landing #5288: JS (`js/hang/src/container/consumer.ts`) still
delivers a group that arrives below the cursor as a late backfill; Rust
(`rs/moq-mux/src/container/consumer.rs`) never does.

- ✅ Decided 2026-10-13: both drop, matching Rust. Not chosen: both deliver,
  flagged so readers can skip; leave it open.
- Match Rust's startup first. Groups are served newest-first, so the head of a
  retained window can arrive after the live edge; JS sets its cursor from the
  first arrival, so dropping alone would lose valid startup media
  (`consumer.outoforder.test.ts` covers that order). Rust keeps it by waiting
  for an explicit start floor under the max-delay budget
  (`startup_keeps_late_arriving_head_group_within_max_delay`); give JS the same
  startup so the window head is never below the cursor.
- Open: a subscription with only a max delay and no start (as in
  `consumer.outoforder.test.ts`) has no floor to wait for, so the window head
  can still land below the cursor, in Rust too. Options include requiring a
  start floor for retained windows or settling the floor before the first
  delivery. Ask the maintainer before dropping groups in that case; do not
  ship a drop that loses startup media.
- Then change the JS consumer to discard groups below the cursor, and add a
  test mirroring Rust's: a reordered group older than the cursor is never
  yielded and no extra `Gap` appears.
- Check JS readers that relied on backfill (watch's audio/video decoders,
  `Sync`, the arrivals handle from #5311 if it has landed) and adjust them.

Public API: no type change; JS read behavior changes. Wire: none.
