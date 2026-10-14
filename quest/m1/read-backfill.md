# [M] Rust and JS container reads drop groups below the cursor

## Goal

The Rust and JS container consumers agree on groups that arrive below the read
cursor: neither delivers them. The read stays `Frame | Gap | End` in delivery
order past the cursor, and what lies below is already covered by the `Gap`
that moved the cursor. This is the ordered read only: APIs that yield in
arrival order keep every group.

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
- ✅ Decided 2026-10-13: a settlement window, for ordered APIs only. An
  ordered read (the container consumer's in-order `Read` stream, Rust and JS)
  with no declared start holds its first delivery until the newest content is
  a full max-delay budget ahead (the test Rust already uses to give up on a
  missing group), takes the lowest group seen as its floor, then drops
  anything below. Unordered, arrival-order APIs (raw `recv_group`, the
  arrivals handle from #5311, anything that yields in arrival order) get no
  window and no drop. Not chosen: knowingly drop, with the floor at the first
  arrival; require a declared start floor for retained windows.
- Give both consumers the window where they lack it (Rust's no-start startup
  adopts its cursor from the first arrival today), then make JS discard groups
  below the cursor. `consumer.outoforder.test.ts`'s case must keep passing
  under the window: the head arriving after the live edge is still delivered.
  Add a test mirroring Rust's: a reordered group below the settled floor is
  never yielded and no extra `Gap` appears.
- Check JS readers that relied on backfill (watch's audio/video decoders,
  `Sync`) and adjust them; one that wants every group reads an arrival-order
  API instead.

Public API: no type change; the ordered read changes in Rust and JS (the first
delivery waits for the window when no start is declared, and JS drops
backfill). Wire: none.
