# [M] Public model after ranges, handover, and the JS tail

## Goal

The hidden model items that other quests reshape are public, unnecessary,
or marked private once those quests land: fetch ingest and answering, the
IETF cache peeks, JS `Track.Subscriber.fork` and `Group.Producer.mirror`,
and JS `hooks.endTrack`. A custom session then answers fetches and track
info, and declares a received end in JS, through public calls only.

## Plan

Split from the [ingest](/quest/m1/public-model/ingest.md),
[serving](/quest/m1/public-model/serve.md), and
[answering](/quest/m1/public-model/answer.md) quests in review of #5717,
so those three do not wait on the large quests that replace these items.
Rules and the inventory method are in the
[epic](/quest/m1/public-model/README.md); rerun the inventory first, since
the prerequisites will have deleted or renamed most of these.

- Fetch ingest (Rust `group::Request` fields `state` and `result`,
  `track::TrackState` with `TrackState::abort`, `track::FetchOutcome`,
  `group::Request::finish_track_at`, `track::Consumer::poll_landed`): remove
  the need. Model ranges replaces per-group requests with ranges, and its
  public range answer is the ingest path.
- Cache peeks (Rust `track::Consumer::peek_latest`, `peek_before`,
  `peek_group`, `is_live`; IETF publisher, for Largest Object and FETCH)
  and JS `Track.Subscriber.fork`: remove the need, since IETF FETCH is served
  from ranges and the largest location is public. If either still has a
  caller, bring the shape back to the maintainer rather than publishing
  peeks.
- JS `Group.Producer.mirror` (lite subscriber fetch coalescing): remove the
  need through Model ranges and JS track handover, which owns the JS fan-out
  copy.
- JS track info and fetch answering (`wire.ts` `overrideBroadcastWire`):
  answered through the public `Track.Request` and `Track.Dynamic` range
  requests that Model ranges defines, completing the answering quest's
  `Broadcast.requested` decision.
- JS `hooks.endTrack` (the JS `finish_at_pending`): public under the
  received-end shape the ingest quest settles in Rust, once JS pending tail
  mirrors the hold.

Tests: as in the other children, mock time in each language, with the
built-in sessions moved onto the public calls.

## Required

- [Model ranges](/quest/m1/subscribe-ranges/model.md) - replaces per-group fetch requests, the peeks, and `fork`
- [Live largest location](/quest/m1/largest.md) - the public largest-location stream that replaces `peek_latest` and `is_live` on the IETF Largest Object path
- [JS track handover](/quest/m1/js-group-handover.md) - owns the JS fan-out copy behind `Group.Producer.mirror`
- [JS pending tail](/quest/m1/js-pending-tail.md) - the JS received-end hold behind `hooks.endTrack`
- [Public ingest](/quest/m1/public-model/ingest.md) - settles the received-end shape JS mirrors
- [JS ranges](/quest/m1/subscribe-ranges/js.md) - the JS `Track.Request`/`Track.Dynamic` range requests that answer track info and fetches
- [Public routes and answering](/quest/m1/public-model/answer.md) - the public `Broadcast.requested` handout this completes
