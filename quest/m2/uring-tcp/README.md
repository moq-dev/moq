# Stream sessions on the ring

## Goal

Move the relay's stream-based media path onto the io_uring workers, so a
WebSocket (qmux) session is served from the same pinned thread and the same
ring as a QUIC one. io_uring's advantage over epoll is far larger for TCP than
for UDP, and the WebSocket path is the one place the relay still pays a
syscall per read and per write on the media hot path.

Tokio is not going away. It stays as the relay's general-purpose runtime for
the things that have no business on a pinned thread: the auth API's HTTP
client and its response cache, `iroh`, cert reload, signals, and session
supervision. This line moves the media path, not the control plane.

## Plan

This line starts after the dev merge, on main.

The three quests below ship together as one capability, in order.

The prerequisite that shapes the middle quest: **qmux sessions arrive through
the axum router**. `web.rs` routes `/` and `/{*path}` to
`websocket::serve_ws` (rs/moq-relay/src/web.rs:256-259). The gate is
moq-relay's own `websocket` feature (rs/moq-relay/Cargo.toml:43, which is
what turns on `axum/ws`) plus the runtime `resolved_ws()` check
(web.rs:257), so a WebSocket session is an HTTP upgrade before it is a media
session. There is no moving qmux onto the ring without also running the HTTP
server that upgrades it there. That is not a reason to rewrite axum: hyper is
runtime-agnostic, so implementing `hyper::rt::{Read, Write, Executor}` over
ring TCP streams keeps axum's routers, extractors, CORS, and its WebSocket
upgrade working unchanged.

Measure before porting, the same way `echo_quiche`
(rs/moq-uring/benches/session_lite.rs) gated the UDP path. The ablation's
number is what justifies the rest of the line.

## Quests

- [Ablation](/quest/m2/uring-tcp/ablation.md) - measure ring TCP against tokio
  TCP under the qmux workload before committing to the port
- [Stream](/quest/m2/uring-tcp/stream.md) - a `tcp` module in `moq-uring`, and
  the `hyper::rt` adapters that let axum run on it
- [Relay](/quest/m2/uring-tcp/relay.md) - serve the relay's WebSocket and
  stream listeners from the io_uring workers
