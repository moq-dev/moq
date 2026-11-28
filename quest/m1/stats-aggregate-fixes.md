# [XS] moq-stats stops stalling on a failed node subscribe

## Goal

`moq-stats` keeps reading a node whose stats subscription failed while its
track stays listed. Not here: the stats layout or any new counter.

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

Done in the PR: `finish_at(start)` on the group closed by the next, `Producer::new` and
`Consumer::new` panic on a zero interval or grace, and the peer-fed counter adds saturate.

Reproduction attempt: a node whose track was aborted before the aggregator polled
did not stall (the aborted track is not left listed), so the repro needs a
subscribe that fails while the listing still offers the track. Recommended fix:
`Node` remembers the track name it subscribed to and clears it on the listing's
`Removed`/`Finished`; a subscribe error with that name still listed and no newer
offer departs and re-resolves once (the `rearmed` guard), otherwise it keeps
waiting as today.

Public API: none beyond the loud refusal of zero values. Wire: none.
