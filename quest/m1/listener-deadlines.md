# [M] Every listener bounds its handshake and headers

## Goal

Every relay listener bounds slow peers the way the default runtime does
after the handshake deadline (moq-dev/moq#4612): the io_uring
workers apply `listen.timeout`, the HTTPS listener's HTTP/2 path and the
`[internal]` listener drop a connection with no complete request in flight
for `listen.timeout` (covering slow headers and an idle keep-alive, even one
that ACKs every PING), and the iroh backend honors `quic.keep_alive`.

## Plan

- io_uring (`rs/moq-relay/src/uring.rs` `serve_connection`): bound the
  WebTransport `Request::accept`, `respond`, and `accept_request_lite` by one
  deadline from `listen::Config::resolved_timeout()`, on the worker's own
  timer (`rs/moq-uring/src/timer.rs`), closing with the timeout code. Delete
  the "There is no timeout here" note in `rs/moq-uring/src/quic/web.rs`.
- HTTP: `axum_server` builds hyper with no timer, and #4612's
  `header_read_timeout` is HTTP/1 only. hyper's HTTP/2 PING keep-alive
  (`http2().timer(..)`, `keep_alive_interval`, `keep_alive_timeout`) only
  detects a dead peer: a client that ACKs every PING and never finishes a
  request stays up. So the bound is a per-connection idle deadline: the
  connection closes once it has had no complete request in flight for
  `listen.timeout`, which covers both a trickled HEADERS block and an idle
  keep-alive. hyper has no such knob, so it lives in the relay's serve path
  (`rs/moq-relay/src/listener.rs` and `web.rs`, e.g. an in-flight counter in
  the per-connection service driving that connection's graceful shutdown).
  Set the PING keep-alive too, for dead peers. Apply both to the HTTPS
  listener and the `[internal]` listener, which also gets the HTTP/1 header
  timer.
- iroh (`rs/moq-tokio/src/iroh.rs`): set iroh 1.3's
  `keep_alive_interval` from `quic.keep_alive`, and fix the docs that say iroh
  has no knob (`rs/moq-tokio/src/quic.rs`, `doc/bin/relay/config.md`).
- Tests on a paused clock where the runtime allows: a stalled io_uring
  handshake closes at the deadline, and an HTTP/2 client that ACKs PINGs but
  never sends a request, or trickles one request's headers, is dropped at the
  deadline while one with a request in flight is not.

Public API: none beyond existing settings. Wire: none.

## Required

- moq-dev/moq#4612 merges, adding `listen.timeout` and the HTTP/1 header timer this extends
