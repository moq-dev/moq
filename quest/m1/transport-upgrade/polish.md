# [S] WebSocket upgrade edge cases

## Goal

The Rust WebSocket-to-QUIC upgrade leaves no false warnings and no avoidable
failures: a GOAWAY the peer received is not logged as a send failure, and a
WebSocket handshake that fails after WebSocket won the dial race falls back to
the still-pending QUIC dial instead of failing the connection attempt.

## Plan

- Every upgrade logs `failed to send goaway: transport: connection closed`
  although the server receives it. Find why the send reports failure (likely
  the close racing the flush) and fix it at the source.
- When WebSocket wins the race but its MoQ handshake fails, the pending QUIC
  dial is still alive; use it rather than failing, bounded by the existing
  connect timeout.
- Tests for both, including the deterministic QUIC-held forwarder from #4189.

Public API: none. Wire: none.

## Related

- [#4189](https://github.com/moq-dev/moq/pull/4189) - the WebSocket-to-QUIC upgrade this polish quest follows
