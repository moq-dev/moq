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
- Change the JS consumer to discard groups below the cursor, and add a test
  mirroring Rust's: a reordered group older than the cursor is never yielded
  and no extra `Gap` appears.
- Check JS readers that relied on backfill (watch's audio/video decoders,
  `Sync`, the arrivals handle from #5311 if it has landed) and adjust them.

Public API: no type change; JS read behavior changes. Wire: none.
