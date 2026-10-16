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
- ✅ Decided 2026-10-15 (maintainer, #5148): **effective authorization.**
  An unnamed request is held to the union of session-lifetime auths; a
  request named by its own auth (a moq-transport request token today,
  `auth=id` on lite later) is held to that auth's issued grant alone, and
  ends with it. Peer-issued AUTH_OK grants never authorize incoming
  requests. Preserve renewal (a new auth replacing the old), revocation,
  and expiry. Joining FETCH follows the named
  subscription's live authorization and ends with that subscription;
  standalone FETCH gains no request-token authority. JS mirrors the gate
  structure.
- **Structure only.** Finding new gaps is not the goal, but any request type
  the refactor shows was ungated gets a regression test in the same PR.
  Initial verification already consumes request capacity and observes
  cancellation (#5148); those fixes must not wait here.

Today `auth::Gate` is built inside each handler: the IETF publisher's
SUBSCRIBE, standalone FETCH, and TRACK_STATUS; the IETF subscriber; and the
lite publisher and subscriber. JS checks `#denied` and `#watch` per handler.
A joining FETCH carries its subscription's namespace and observes that
subscription's effective authorization. A surviving cache is not authority
to continue a dependent join after the subscription ends.

Test named admission outside the session union, session-union changes
without revoking a still-valid named grant, revoke/renewal/expiry of a named
auth, and dependent joining FETCH. Keep graceful terminal messages.

## Related

- [JS fetch grant watch](https://github.com/moq-dev/moq/pull/5260) - the outgoing
  side: a JS `fetchGroup` this session issued ends when its path leaves the grant
