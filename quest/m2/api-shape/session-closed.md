# [S] A peer's closed session reads as Closed in the bindings

## Goal

A binding sees `MoqError::Closed` when the session behind a track or group
closed, whether it closed locally or on a peer across a real session. Today
`Closed` comes only from `moq_net::Error::Closed`, `GoingAway`, and a local
`Stream(Session(Cancel))` (`rs/moq-ffi/src/error.rs`, the `moq_net::Error`
conversion). A peer's SESSION_CLOSED stream reset decodes to
`StreamError::Session(SessionError::Internal)` on both moq-lite
(`rs/moq-net/src/error.rs`) and moq-transport (`rs/moq-net/src/ietf/error.rs`),
so a binding sees `Protocol { kind: SessionClosed, code: 0x3 }` on either wire.
moq-transport does not turn it into `Internal`. No test on main checks this
path: the one related test, `test_tcp_rst_and_endpoint_close_are_errors`
(`py/moq-rs/tests/test_socket.py`), is skipped as flaky and covers endpoint
close, not a peer.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete follow-up of
[moq-dev/moq#8373](https://github.com/moq-dev/moq/pull/8373), which guarantees
`Closed` only for two endpoints sharing one in-process origin). ✅ m2, in this
epic, since it changes the bindings' public errors (rejected: m1 as additive;
dropping it and documenting the difference).

Map a received SESSION_CLOSED to `Closed` in moq-ffi's `From<moq_net::Error>`,
with a `Stream(StreamError::Session(_))` arm beside the local one. Both wires
already decode to that variant, so no moq-net decoding change is needed. Also
fix the comment at `rs/moq-ffi/src/error.rs` (near line 428) that says a remote
SESSION_CLOSED decodes as `Internal`, and the test `local_session_close_is_closed`,
which pins a received SESSION_CLOSED to `Protocol`.

A closed session also ends its tracks with `Error::Session(..)`
(`rs/moq-net/src/lite/session.rs` near line 481, `rs/moq-net/src/ietf/session.rs`
near line 533), which the stream arm does not match. Before the change, confirm
what a reader of a peer-closed session gets on each path, and map each path
that means the session closed to `Closed`.

Check whether `js/net` exposes the same split and mirror the result there, per
the cross-package rule. `rs/moq-c` has no `Closed` kind; decide whether it gains
one with the bindings. Its Impact section says how binding callers that match
`SessionClosed` migrate.

Public API: breaking for the bindings' error kinds. Wire: none.
