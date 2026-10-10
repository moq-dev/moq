# Stream sessions on the ring

## Goal

Move the relay's stream-based media path onto the io_uring workers, so a
WebSocket (qmux) session is served from the same pinned thread and the same
ring as a QUIC one. io_uring's advantage over epoll is far larger for TCP than
for UDP, and the WebSocket path is the one place the relay still pays a
syscall per read and per write on the media hot path.

Tokio is not going away. It stays as the relay's general-purpose runtime for
the things that have no business on a pinned thread: the relay's
`moq_auth::Client`, `iroh`, cert reload, signals, and session
supervision. This line moves the media path, not the control plane.

## Plan

The three quests below ship together as one capability, in order.

The prerequisite that shapes the middle quest: **qmux sessions arrive through
the axum router**. `rs/moq-relay/src/web.rs` routes `/` and `/{*path}` to
`websocket::serve_ws`. The gate is moq-relay's own `websocket` feature
(which turns on `axum/ws`) plus the runtime `resolved_ws()` check, so a
WebSocket session is an HTTP upgrade before it is a media
session. There is no moving qmux onto the ring without also running the HTTP
server that upgrades it there. That is not a reason to rewrite axum: hyper is
runtime-agnostic, so implementing `hyper::rt::{Read, Write, Executor}` over
ring TCP streams keeps axum's routers, extractors, CORS, and its WebSocket
upgrade working unchanged.

Measure before porting, the same way the echo bench
(rs/moq-uring/benches/session_lite.rs) gated the UDP path. The ablation's
number is what justifies the rest of the line.

Decided in the 2026-09-30 audit: moved to m2. The
[transport upgrade](/quest/m1/transport-upgrade/README.md) shrinks the
WebSocket hot path by moving clients to QUIC, and no fleet demand asks for
ring TCP.

Decided 2026-10-08: moved to m3. No fleet demand asks for ring TCP; the
ablation stays the gate for the rest of the line.

## Required

- [Ablation](/quest/m3/uring-tcp/ablation.md) - measure ring TCP against tokio
  TCP under the qmux workload before committing to the port
- [Stream](/quest/m3/uring-tcp/stream.md) - a `tcp` module in `moq-uring`, and
  the `hyper::rt` adapters that let axum run on it
- [Relay](/quest/m3/uring-tcp/relay.md) - serve the relay's WebSocket and
  stream listeners from the io_uring workers
