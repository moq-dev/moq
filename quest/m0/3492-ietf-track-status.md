# [M] moq-net: TRACK_STATUS gets a truthful answer on every draft

## Goal

A moq-transport peer that sends TRACK_STATUS receives an answer: the track's
status and largest location when the publisher serves it, and an error when
it does not, on draft-14 through draft-20, from the Rust and the JavaScript
publisher alike. Today Rust logs a warning and drops the stream without
decoding the request, and on draft-14/15 the virtual stream reset is a no-op
so zero bytes reach the peer, which waits out its timeout. js/net replies but
never consults the broadcast, answering NOT_FOUND or a bare REQUEST_OK for
tracks it is serving.

## Plan

Branch from main; the wire additions are within the drafts already
negotiated.

- `rs/moq-net/src/ietf/publisher.rs` replaces the no-op arm with
  `run_track_status_stream`: decode the request (`ietf::TrackStatus` in
  `track.rs` already round-trips v14 through v18), resolve the track through
  the same `request_broadcast` / `track` / `subscribe` path `run_subscribe`
  uses so a routed broadcast attaches its route, read the live edge with the
  existing `live_edge` helper, reply, and drop the subscription.
- Replies: draft-14 sends TRACK_STATUS (0x0E) with `TrackStatusCode` and the
  last group and object, giving that enum its first real use; 0x0E also
  numbers `NamespaceDone` on v16, so note the per-version collision. Draft-15
  and later send REQUEST_OK carrying the draft's TRACK_STATUS parameters,
  which `RequestOk::encode_msg` has to grow, and REQUEST_ERROR with the
  registered code when the track is unknown or unauthorized. Close the writer
  explicitly, as `run_publish_stream` does, so drop-time reset cannot discard
  the reply.
- `js/net/src/ietf/publisher.ts` `runTrackStatusRequest` answers from the
  broadcast it serves with the same status and location, and its v15+ branch
  agrees with Rust on the error reply, so the interop matrix cannot disagree.
- Tests: byte-exact transport-log tests per version in the
  `run_publish_stream` style for a served track, a missing track, and the
  draft-15 virtual-stream path where nothing is emitted today; the same in
  `publisher.test.ts`. Run the interop runner's track-status case if one
  exists.

## Closes

- [#3492](https://github.com/moq-dev/moq/issues/3492) - close this issue when the quest finishes

## Related

- [IETF error codes](/quest/m0/ietf-error-codes.md) - the registry type the error reply should use once it lands; not a blocker
- [Status forwarding](/quest/m3/ietf-track-status-forward.md) - answering without attaching a route
