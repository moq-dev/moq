# [M] Every incoming request is held to the grant by construction

## Goal

Every request a peer sends, on lite and IETF, in Rust and `@moq/net`, is
checked against the grant when it is dispatched and held to it for its whole
life: resolution, reads, and writes. A new request type cannot be served
without a gate. Per-handler checks already missed standalone FETCH and
TRACK_STATUS in Rust and the TRACK re-check and setup announces in JS, all
fixed in #4039, as were the joining FETCH and the TRACK_STATUS and TRACK_INFO
answers while they are written. Behavior stays the same: no wire or public API
change, and the existing auth tests are the regression net.

## Plan

Decisions settled while planning (2026-10-09):

- **The dispatcher owns the gate.** It refuses an out-of-grant request up
  front, the same way for every type, and hands the live gate to the handler,
  whose signature requires one. The handler still picks its graceful end on a
  narrowed grant (PUBLISH_DONE `Unauthorized`, a REQUEST_ERROR, or a stream
  reset once FETCH_OK is out). Racing the whole handler future and dropping it
  was rejected because it loses those graceful refusals.
- **Rust and JS together**, so the shape and names stay mirrored.
- **Structure only.** Finding new gaps is not the goal, but any request type
  the refactor shows was ungated gets a regression test in the same PR.

Today `auth::Gate` is built inside each handler: the IETF publisher's
SUBSCRIBE, standalone FETCH, and TRACK_STATUS; the IETF subscriber; and the
lite publisher and subscriber. JS checks `#denied` and `#watch` per handler.
A joining FETCH carries its subscription's namespace and holds its own gate,
since the cache it answers from outlives the subscription.

## Related

- [JS fetch grant watch](/quest/m1/auth/js-fetch-watch.md) - the outgoing
  side: a JS `fetchGroup` this session issued ends when its path leaves the grant
