# [S] Handshakes and HTTP headers have deadlines

## Goal

A connection that never completes its QUIC, WebTransport, WebSocket or MoQ
SETUP handshake is closed after a deadline, and the relay's HTTP listener
times out slow request headers. Today `moq-tokio/src/server.rs` pushes
`accept_request` futures with no deadline, and the 5 s keep-alive keeps an
idle pre-SETUP connection alive indefinitely. axum-server 0.8 builds hyper
without a timer, so hyper's `header_read_timeout` never fires
(`moq-relay/src/web.rs`).

## Plan

- Wrap the accept future in a timeout covering everything up to a completed
  SETUP, then close with the transport's timeout code. The timeout is a
  config value with a default of around 10 s.
- Configure `http1().timer(TokioTimer).header_read_timeout(..)` on the
  relay's HTTP builder.
- Tests on a paused clock: a stalled SETUP is closed at the deadline, and a
  completed one is not.
- Document both in `doc/bin/relay/config.md`.

Public API: a config field on the server. Wire: none.
