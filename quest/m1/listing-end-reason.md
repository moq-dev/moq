# [M] A listing says whether a removed track finished

## Goal

A track listing's removal says whether the track finished or left unfinished
(its producer dropped or aborted, or its broadcast's route withdrawn), at every
hop. The Rust moq-transport answerer then ends a finished track's
SUBSCRIBE_TRACKS PUBLISH with TRACK_ENDED, so a Rust-to-Rust IETF listing
([moq-dev/moq#6280](https://github.com/moq-dev/moq/pull/6280)) removes a
finished track quietly. An unfinished one keeps ending in error, which fails
the listing loudly as today.

Today the answerer cannot tell: `TracksEvent::Removed` carries only the name,
and the instance that left is gone from the front the answerer reads through,
so it ends every unlisted PUBLISH in error and each removal resets the listing.
Decided by the maintainer on 2026-10-22: outbound removes only on TRACK_ENDED,
and the answerer sends TRACK_ENDED only for a track that really finished.

## Plan

Decided 2026-10-22 (maintainer), public API and wire:

- Rust and JS gain a `TracksEvent::Finished { name }` variant beside
  `Removed { name }`, which now means unfinished, and `Lister::finish(name)`
  beside `remove`. Rejected: a `finished: bool` flag, and an `End` enum on
  `Removed`.
- lite-07 gains a TRACKS_FINISH event type on the Tracks Stream, with the same
  body as TRACKS_REMOVE, so a relay chain keeps the distinction. TRACKS_REMOVE
  keeps its meaning. Rejected: a reason field on TRACKS_REMOVE. Update
  `drafts/draft-lcurley-moq-lite.md` and js/net in the same change.
- It lands before the lite-07 freeze: [Finalize moq-lite-07](/quest/m1/lite07-finalize.md)
  Requires it.
- A relay's copy reports what its upstream reported, never what its cache
  happens to know.
- Test Rust-to-Rust over drafts 18-22: a finished track is removed, an
  aborted one fails the listing.

## Related

- [Broadcast archives](/quest/m1/archive/README.md) - lists an IETF peer's tracks through SUBSCRIBE_TRACKS
