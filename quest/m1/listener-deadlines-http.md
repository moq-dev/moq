# [M] HTTP listeners drop a connection with no request in flight

## Goal

The HTTPS listener's HTTP/2 path and the `[internal]` listener drop a
connection with no complete request in flight for `listen.timeout`, covering
slow headers and an idle keep-alive, even one that ACKs every PING.

## Plan

- `axum_server` builds hyper with no timer, and #4612's
  `header_read_timeout` is HTTP/1 only. hyper's HTTP/2 PING keep-alive
  (`http2().timer(..)`, `keep_alive_interval`, `keep_alive_timeout`) only
  detects a dead peer: a client that ACKs every PING and never finishes a
  request stays up. So the bound is a per-connection idle deadline: the
  connection closes once it has had no complete request in flight for
  `listen.timeout`, which covers both a trickled HEADERS block and an idle
  keep-alive. hyper has no such knob, so it lives in the relay's serve path
  (`rs/moq-relay/src/listener.rs` and `web.rs`, e.g. an in-flight counter in
  the per-connection service driving that connection's graceful shutdown).
- Set the PING keep-alive too, for dead peers. Apply both to the HTTPS
  listener and the `[internal]` listener, which also gets the HTTP/1 header
  timer.
- Tests on a paused clock: an HTTP/2 client that ACKs PINGs but never sends a
  request, or trickles one request's headers, is dropped at the deadline
  while one with a request in flight is not.

Split from [io_uring handshake deadline](/quest/m1/listener-deadlines.md) on
2026-10-08.

Public API: none beyond existing settings. Wire: none.
