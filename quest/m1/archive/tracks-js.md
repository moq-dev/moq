# [M] TRACKS in js/net

## Goal

`@moq/net` speaks the moq-lite-07 Tracks Stream (0x8) that Rust already
speaks, with the same behavior and mirrored names:

- **Listing.** A broadcast consumer lists its tracks (Rust
  `broadcast::Consumer::tracks`): each listed track with its info, one
  `synced` event, then adds and removals until the broadcast ends. Over a
  session it opens one TRACKS per broadcast however many readers list it,
  syncs its readers at once, and forwards the peer's tracks as live adds.
- **Answering.** A session answers a peer's TRACKS for a broadcast it
  publishes with the tracks its producer created (never one served only on
  request), using the per-reader queue and SYNCED rule the draft specifies.
- **Refusals.** Older versions reset the stream type, as now. A covering
  prefix route answers NOT_FOUND. A listing past the session's announce cap
  is reset.

## Plan

The wire and the Rust model landed first; read the Tracks section of
`drafts/draft-lcurley-moq-lite.md` and mirror `rs/moq-net/src/model/listing.rs`
(the queue keyed by name in insertion order, where an unread add and removal
cancel and a removal then re-add of a seen name collapses in place) and
`rs/moq-net/src/lite/tracks.rs` (the encoding: TRACKS_ADD 0x0 with an
embedded TRACK_INFO, TRACKS_REMOVE 0x1, SYNCED 0x4).

`js/net/src/lite/malformed.test.ts` still treats type 8 as unknown on
lite-07; it becomes a real stream here.

Test the same cases as Rust: snapshot then SYNCED, SYNCED after N events
under churn, a stalled reader's bounded queue, removal before SYNCED, a
dynamic-only track never listed, the broadcast ending the listing, and
`just test interop --all` against the Rust side in both directions.

Public API: a track-listing method on the JS broadcast consumer, named like
Rust's. Wire: none new.
