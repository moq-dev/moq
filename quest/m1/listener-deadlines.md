# [S] io_uring workers bound the handshake

## Goal

The io_uring workers bound a slow handshake the way the default runtime does
after the handshake deadline (moq-dev/moq#4612), by applying
`listen.timeout`.

## Plan

- `rs/moq-relay/src/uring.rs` `serve_connection`: bound the
  WebTransport `Request::accept`, `respond`, and `accept_request_lite` by one
  deadline from `listen::Config::resolved_timeout()`, on the worker's own
  timer (`rs/moq-uring/src/timer.rs`), closing with the timeout code. Delete
  the "There is no timeout here" note in `rs/moq-uring/src/quic/web.rs`, and
  the matching caveat in `doc/bin/relay/config.md`.
- Test on the worker's timer: a stalled handshake closes at the deadline.

Split on 2026-10-08: the HTTP/2 idle deadline is
[HTTP listener deadlines](/quest/m1/listener-deadlines-http.md) and the iroh
keep-alive is [iroh keep-alive](/quest/m1/iroh-keep-alive.md); each lands on
its own.

Public API: none beyond existing settings. Wire: none.

## Related

- [HTTP listener deadlines](/quest/m1/listener-deadlines-http.md) - the HTTPS and internal listeners' half
- [iroh keep-alive](/quest/m1/iroh-keep-alive.md) - the iroh backend's half
