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

- Public API and wire, so settle the shape with the maintainer first: how
  `TracksEvent::Removed` and `broadcast::Lister::remove` carry the end, and a
  lite-07 TRACKS_REMOVE field so a relay chain keeps it (`drafts/` and js/net
  in the same change).
- A relay's copy reports what its upstream reported, never what its cache
  happens to know.
- Test Rust-to-Rust over drafts 18-22: a finished track is removed, an
  aborted one fails the listing.

## Related

- [Broadcast archives](/quest/m1/archive/README.md) - lists an IETF peer's tracks through SUBSCRIBE_TRACKS
- [Pause an IETF SUBSCRIBE](/quest/m1/ietf-subscribe-pause.md) - the other gap in a clean Rust-to-Rust IETF listing
