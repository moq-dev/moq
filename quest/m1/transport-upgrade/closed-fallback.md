# [S] Fall back to QUIC when the WebSocket session closes at once

## Goal

When WebSocket wins the dial race and its session closes right after
connecting, the `Connection` moves onto the QUIC dial that is still pending
instead of dropping it and redialing. On moq-lite and IETF draft 17+ the
client handshake does not wait for the server, so a server refusing the
WebSocket session looks like this rather than like a handshake failure, which
already falls back.

## Plan

- Today `run_session` in `moq-tokio`'s `connection.rs` returns when the
  WebSocket session closes, and the pending upgrade is dropped. The redial
  starts a fresh QUIC dial and has lost QUIC's head start.
- Decide what counts as "at once". The unhealthy-session threshold that
  already feeds the backoff may be the right line. Keep the connect deadline
  that bounds the pending dial as the only deadline.
- An auth close on the WebSocket session is probably still terminal, the way
  it is for a lone session. Check how the race treats a mixed auth pair and
  match it.
- Test with the held-QUIC forwarder in the connection tests, as the handshake
  fallback test does, with a default moq-lite client.

Public API: none. Wire: none.

## Related

- [#4296](https://github.com/moq-dev/moq/pull/4296) - the handshake-failure fallback this extends
