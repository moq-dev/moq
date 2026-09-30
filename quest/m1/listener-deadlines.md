# [M] Every listener bounds its handshake and headers

## Goal

Every relay listener bounds slow peers the way the default runtime does
after the handshake deadline (moq-dev/moq#4612): the io_uring
workers apply `listen.timeout`, the HTTPS listener's HTTP/2 path and the
`[internal]` listener time out slow headers and idle keep-alive, and the iroh
backend honors `quic.keep_alive`.

## Plan

- io_uring (`rs/moq-relay/src/uring.rs` `serve_connection`): bound the
  WebTransport `Request::accept`, `respond`, and `accept_request_lite` by one
  deadline from `listen::Config::resolved_timeout()`, on the worker's own
  timer (`rs/moq-uring/src/timer.rs`), closing with the timeout code. Delete
  the "There is no timeout here" note in `rs/moq-uring/src/quic/web.rs`.
- HTTP: `axum_server` builds hyper with no timer. On the HTTPS listener set
  `http2().timer(..)`, `keep_alive_interval`, and `keep_alive_timeout`
  (hyper-util 0.1.21); give the `[internal]` listener the same header timer
  #4612 gives HTTP/1.
- iroh (`rs/moq-tokio/src/iroh.rs`): set iroh 1.3's
  `keep_alive_interval` from `quic.keep_alive`, and fix the docs that say iroh
  has no knob (`rs/moq-tokio/src/quic.rs`, `doc/bin/relay/config.md`).
- Tests on a paused clock where the runtime allows: a stalled io_uring
  handshake closes at the deadline, and a slow HTTP/2 client is dropped.

Public API: none beyond existing settings. Wire: none.

## Required

- moq-dev/moq#4612 merges, adding `listen.timeout` and the HTTP/1 header timer this extends
