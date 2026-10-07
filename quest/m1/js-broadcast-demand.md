# [XS] JS broadcast demand matches Rust

## Goal

`@moq/net`'s `Broadcast.Demand` (`js/net/src/broadcast.ts`) agrees with Rust's
broadcast demand in the three cases where it differs today, each with a test:

- A pending `resolveTrackInfo` query counts as demand, as a pending track
  request does in Rust.
- `removeTrack` stops counting the removed track at once, instead of until it
  closes.
- Inserting an already-closed track leaves no cleanup entry behind.

## Plan

Found while landing #4708, which introduced `Broadcast.Demand` and per-level
`demand()` handles in both languages. Nothing outside the bench reads
`Broadcast.Demand` yet, so this is parity, not a live bug.

Public API: none beyond the corrected behavior. Wire: none.
