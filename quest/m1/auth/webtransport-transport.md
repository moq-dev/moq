# [S] WebTransport transport

## Goal

An auth decider tells a WebTransport session from a native QUIC one:
`moq_auth::Transport::WebTransport` (`"webtransport"` on the wire) for a
session that arrived over WebTransport, and `Transport::Quic` (`"quic"`) only
for native QUIC. Every accept path sets it: moq-tokio's noq backend, moq-uring,
and the relay's `request_for`. An auth server reading the request JSON sees the
new value.

## Plan

Decided 2026-10-07, for moq.pro's per-transport session stats (a customer
measuring how often browsers fall back to WebSocket, whose native QUIC
workers share the `quic` count today):

- **A variant, not a flag.** Rejected: an additive `webtransport: bool` beside
  `transport: "quic"`. The variant states the truth, and the enum is already
  `#[non_exhaustive]`. An auth server that matches `"quic"` sees
  `"webtransport"` for browser sessions after this lands: call it out in the
  changelog.
- **The ALPN cannot tell them apart.** Both present the negotiated
  sub-protocol (e.g. `moq-lite-04`) as `alpn`; `h3` never reaches auth. The
  signal is the server request itself: `server::Request` has a URL only on
  WebTransport (`rs/moq-tokio/src/noq.rs`, `rs/moq-uring/src/quic/web.rs`).
  Carry the transport on `server::Request` rather than inferring it from the URL
  at each call site.
- Update `Transport::Quic`'s doc, which says it covers WebTransport, and any JS
  or binding mirror of the enum.
- Tests: a WebTransport session and a native QUIC session each reach the
  decider with their own transport, on the noq and uring backends.

## Related

- [Session outcomes](/quest/m1/session-outcomes.md) - the same consumer's
  refusal and end counters
- [Transport upgrade](/quest/m1/transport-upgrade/README.md) - a session that
  moves from WebSocket to QUIC; whether its transport is re-reported is that
  line's call
