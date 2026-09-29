# [M] Lite request streams stop when the requester leaves

## Goal

A lite publisher stops resolving a SUBSCRIBE or FETCH as soon as the
requester closes its send direction, by FIN or reset, as TRACK already does
since #4494 (the draft ends a transaction when either side closes), so an
abandoned request releases its upstream lookup along the whole mesh. Serving
a request stream can't forget that watch: the handlers for TRACK, SUBSCRIBE,
and FETCH never hold the reader while they wait.

## Plan

`TrackInfoServe`, `SubscribeServe`, and `FetchServe` in
`rs/moq-net/src/lite/publisher.rs` each repeat the same decode, origin, and
broadcast steps, the same error classification and `abort`, and only
`TrackInfoServe` watches the reader for a FIN or reset. SUBSCRIBE waiting in
`Request` or `Confirm`, and FETCH waiting in `Request` or `Fetch`, pin the
upstream lookup for a requester that already left.

Share one request-stream wrapper that owns the reader: it decodes the
message, then drives a per-kind handler that gets only the writer, watching
the reader for a FIN or reset for the whole wait. Pending data is not a close:
SUBSCRIBE forwards each SUBSCRIBE_UPDATE to its handler, and only a FIN or a
read error ends the handler. Fold the origin and broadcast steps into one shared
resolve step, so each state enum loses its `Hop` and `Request` variants.

Rejected: a generic `kio::until(task, scope)`. The interest signal is owned
by the task itself (the reader, a `track::Request`), it covers one phase
rather than the whole task, and cancelling a track is a commit that can lose
to returning demand (`reject_unused`). Those are moq-net concerns, so the
guarantee comes from ownership in moq-net, not a kio combinator. The lite and
IETF subscribers keep their inline `poll_unused` checks.

Tests: a SUBSCRIBE and a FETCH whose requester FINs or resets while the publisher
waits on the broadcast leave the upstream request unused, over real lite-05
and lite-06 sessions like `tests/track_info_cancel.rs`.

Public API: none. Wire: none.

## Related

- [Cross-relay bursts](/quest/m1/cross-relay-bursts.md) - unanswered FETCHes across relays, where an abandoned request pinning its lookup would show up
