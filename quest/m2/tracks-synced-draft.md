# [XS] The lite-07 draft's TRACKS SYNCED matches relays

## Goal

The moq-lite draft's Tracks Stream section says what Rust and JS relays do:
a relay holding a listing fed by its upstream sends SYNCED only after that
upstream's SYNCED, and one with no listing source sends it at once. SYNCED
then always means the subscriber holds the complete list. Not here: any
code change.

## Plan

From the moq.pro stats and billing audit
([moq-dev/moq.pro#4513](https://github.com/moq-dev/moq.pro/pull/4513)
follow-ups). `drafts/draft-lcurley-moq-lite.md` (Tracks Stream, around
`:680`) says a relay "never waits on its upstream" and sends SYNCED at once,
with upstream tracks following as live TRACKS_ADD. The code waits:
`ListingState::synced()` is `!self.lister || self.answered`
(`rs/moq-net/src/model/listing.rs`), and the upstream forwarder marks the
lister synced only on the peer's SYNCED (`rs/moq-net/src/lite/subscriber.rs`,
`js/net/src/lite/subscriber.ts`, `js/net/src/listing.ts`).

Decided 2026-11-26 (maintainer, /quest-plan of the moq.pro#4513
follow-ups): ✅ change the draft to match the code (rejected: make relays
send SYNCED at once, which leaves a subscriber unable to tell when it has
the full list; dropping it).

Reword the "relative to this hop" paragraph and keep the SYNCED bound
("no more events before SYNCED than it listed then") consistent with a
relay that waits. Check the paragraph against both implementations before
changing it.

Public API: none. Wire: none (documents lite-07-wip as built).
