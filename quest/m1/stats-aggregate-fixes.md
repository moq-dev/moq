# [S] moq-stats stops stalling on a failed node subscribe and rejects zero settings

## Goal

`moq-stats` keeps reading a node whose stats subscription failed while its
track stays listed, ends the producer's delta groups with GROUP_END when the
next group's start is known, rejects a zero publish interval or a zero grace
at setup, and never panics or wraps on counters a peer sends. Not here: the
stats layout or any new counter.

## Plan

From the moq.pro stats and billing audit
([moq-dev/moq.pro#4513](https://github.com/moq-dev/moq.pro/pull/4513)
follow-ups), checked against `main` at `b97c58535f`. Line numbers there
were stale; the ones below are current.

Decided 2026-11-26 (maintainer, /quest-plan of the moq.pro#4513
follow-ups): ✅ fix the real bugs and the edge cases (rejected: only the
stall and `finish_at`; dropping it). Refuted, so not planned: the
`Snapshot::update` panic after a delta write error (the dropped pending
frame resets the encoder, so the next update is a snapshot), `Drain`
wall-clock drift (stamps map through `kio::clock::origin()` plus monotonic
elapsed), and the same-tick reclaim and re-adopt collision.

1. **Waiting stall** (`rs/moq-stats/src/aggregate.rs`, the
   `Reader::Subscribing` error arm around `:1158`): a subscribe error while
   the listing is alive sets `Reader::Waiting`, which resumes only when a
   later `TracksEvent::Added` sets `node.offered`. If the track stays listed,
   the node is never read again and its contribution freezes. Reproduce it
   in a test first. Recommended: resume on a MoQ signal, such as departing
   and re-resolving the node as the arm without a listing does (its `rearmed`
   guard stops a hot loop), not a timer.
2. **GROUP_END** (`rs/moq-stats/src/produce.rs` around `:653`): the group
   closed because the next one starts should use `finish_at(start)`, so a
   lite-07 reader accounts the span. The other two `finish()` calls (around
   `:668` and `:672`) have no next start and stay.
3. **Zero settings**: `Config::with_interval(Duration::ZERO)` (documented
   non-zero) divides by zero in `Drain::stamp`, and `grace == 0` folds an
   adopted restart's counters and then re-adds them. Nothing sets either
   today (the relay rejects a zero interval), so recommended: refuse both
   loudly where the producer and aggregator are built rather than adding
   machinery for them.
4. **Counter overflow** (`rs/moq-net/src/stats.rs`, `Content::add`,
   `Traffic::add`, `Presence::add`): the `u64` fields are deserialized from
   peers, so a hostile node's counters can overflow the aggregate's `+=`
   (a panic in debug, a wrap in release). Use saturating adds.

Public API: none beyond the loud refusal of zero values. Wire: none.
