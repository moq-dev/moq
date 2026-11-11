# [S] A WebTransport client always reads the peer's close reason

## Goal

When a peer closes a WebTransport session, the client's error carries the
peer's close code and reason, never `cancelled`, whichever arrives first: a
stream reset or the CLOSE_WEBTRANSPORT_SESSION capsule. That covers every
handshake refusal, such as a version mismatch or a refused token, in
`moq-tokio` and `moq-uring`.

## Plan

Follow-up of [moq-dev/moq#8478](https://github.com/moq-dev/moq/pull/8478):
the relay closes the session with the version code (0x15), but its SETUP
stream's reset (code 0) reaches the client before the capsule, so the client
reports `cancelled`. Closing the session before releasing its streams won the
race only sometimes, so ordering on the sender is not the fix.

Recommended shape (draft-ietf-webtrans-http3 §4.4): a closing session resets
and stops its open streams with WT_SESSION_GONE, and a reader that sees
WT_SESSION_GONE before the capsule waits for it, ending on the connection's
close if none comes. Reading WT_SESSION_GONE already exists
(`rs/moq-tokio/src/quic/web/error.rs`, `session_gone`), but it reports
`Error::SessionGone` until the capsule lands, and nothing sends it. The same
code lives in `rs/moq-uring/src/quic/web/`; keep the two in step, and check
whether `js/net`'s native WebTransport path has the same race.

Test the race both ways on the paused clock: the reset first and the capsule
first both yield the peer's code and reason.

Decided 2026-11-11 (maintainer, follow-up planning in /quest-complete): ✅ m1
(rejected: m3, drop): a non-breaking fix that every handshake refusal's
diagnosis needs.

Public API: none. Wire: WT_SESSION_GONE on stream resets, as the WebTransport
draft specifies.
