# [S] JS readers hold for a track's pending tail

## Goal

A `@moq/net` reader of a received track waits for missing groups below a
declared end until the tail settles, as Rust readers do since #4225,
instead of ending at the end with groups missing (`doc/lib/js/net.md`).

## Plan

Mirror the Rust pending-tail hold from #4225 (`rs/moq-net/src/model/track.rs`)
and test it against the Rust behavior with mocked time. Split from [JS session parity](https://github.com/moq-dev/moq/pull/5214) on
2026-10-08 so the caps work stays ready.

Mirror the Rust arrival record rather than #4225's cache scan (decided
2026-10-10): `TrackState::arrivals` in `rs/moq-net/src/model/track.rs`, a
`Ledger` (`rs/moq-net/src/model/ledger.rs`) of the live groups and datagrams
that arrived, which eviction never touches. A gap between two arrivals folds
once it is a subscription's grace old, the same rule as the session's
`tail::Tail`. A reader whose pending tail still owes a group when the track
aborts gets the abort error, not a clean end.

Public API: none. Wire: none.

## Related

- [JS session parity](https://github.com/moq-dev/moq/pull/5214) - per-session caps, the other half
