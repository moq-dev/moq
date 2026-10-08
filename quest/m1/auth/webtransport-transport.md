# [S] WebTransport transport

## Goal

An auth decider tells a WebTransport session from a native QUIC one:
`moq_auth::Transport::WebTransport` (`"webtransport"` on the wire) for a
session that arrived over WebTransport, and `Transport::Quic` (`"quic"`) only
for native QUIC. Both relay accept paths set it: the tokio listener's
`request_for` and the uring listener. An auth server reading the request JSON
sees the new value.

## Plan

Decided 2026-10-07, for moq.pro's per-transport session stats (a customer
measuring how often browsers fall back to WebSocket, whose native QUIC
workers share the `quic` count today):

- **A variant, not a flag.** Rejected: an additive `webtransport: bool` beside
  `transport: "quic"`. The variant states the truth. An auth server that
  matches `"quic"` sees `"webtransport"` for browser sessions after this
  lands: call it out in the changelog.
- **Auth servers upgrade before relays** (decided 2026-10-08).
  `#[non_exhaustive]` does not help the wire: `moq_auth::Transport` has no
  unknown-value fallback and `moq auth serve` deserializes the whole request
  (`rs/moq-auth/src/serve.rs`), so an auth server on today's moq-auth rejects
  `"webtransport"`, the relay reads that as Unavailable, and every browser
  session is refused. Auth servers, moq.pro included, deploy the new moq-auth
  first; document the order in the changelog and `doc/bin/relay/auth.md`.
- **Add a `#[serde(other)]` fallback** to `moq_auth::Transport` in the same
  change, so the next variant reads as unknown on an older server instead of
  failing the request. Test that an unknown transport deserializes to it.
  `@moq/auth`'s `TransportSchema` (`js/auth/src/contract.ts`) is a closed
  `z.enum`; give it the same catch-all.
- **The ALPN cannot tell them apart.** Both present the negotiated
  sub-protocol (e.g. `moq-lite-04`) as `alpn`; `h3` never reaches auth. The
  transport already knows: `moq_tokio::server::Request::transport()` returns
  `Transport::WebTransport`, which `request_for` in `rs/moq-relay/src/auth.rs`
  collapses to `Quic`. Map it to the new variant there. The uring listener
  (`rs/moq-relay/src/uring.rs`) builds its auth request with a hardcoded
  `Quic` (and works out the transport from `url` only after auth); hoist that
  above the auth request.
- Update `Transport::Quic`'s doc, which says it covers WebTransport, and any JS
  or binding mirror of the enum.
- Tests: a WebTransport session and a native QUIC session each reach the
  decider with their own transport, on the tokio and uring listeners.

## Related

- [Session outcomes](/quest/m1/session-outcomes.md) - the same consumer's
  refusal and end counters
- [Transport upgrade](/quest/m1/transport-upgrade/README.md) - a session that
  moves from WebSocket to QUIC; whether its transport is re-reported is that
  line's call
