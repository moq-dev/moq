# [L] qmux drives its own socket

## Goal

A qmux session over TCP, TLS, Unix, or WebSocket is one task that owns its
socket. qmux spawns nothing and depends on no tokio runtime, so the same
session runs on tokio or on an io_uring worker. The bench shows the
before/after on every stream transport, with no wire change.

## Plan

Settled 2026-10-22:

- **Driver API: constructors return `(Session, Driver)`.** `Driver` is a
  future the caller spawns on its own runtime: `tokio::spawn` today, a
  moq-uring local task later. This removes the `tokio::spawn` calls in
  `rs/qmux/src/session.rs` and `transport.rs`, and follows the repo's
  handle-over-callback rule. The driver does not need to be `Send`.
- **IO trait: `tokio::io::{AsyncRead, AsyncWrite}`.** They need no tokio
  runtime, and today's callers already speak them: tokio-rustls, hyper's
  `TokioIo`, and `tokio::net`. The ring's TCP stream implements them too.
- **Relay WebSocket: keep axum and hyper for routing and the 101 response,
  then reclaim the IO.** Use `hyper::upgrade::on` and
  `Upgraded::downcast` to recover the concrete TCP or TLS stream plus any
  bytes hyper already read, and drop axum's `WebSocketUpgrade` and its
  tungstenite 0.29 conversion (`rs/moq-relay/src/websocket.rs`).

What goes, per the audit in the [epic](/quest/m2/perf/stream/README.md):

- the `futures` split BiLock
- the `ws_pump` task and `mpsc(16)`
- the separate `run_reader` and `run_writer` tasks
- `tokio::io::split` and the `reader_loop` task on plain streams
- `tokio::select!` and `tokio::time`. The WebSocket keep-alive moves to
  `kio::clock` like the mux timers.

Buffers: parse inbound records from one reusable read buffer into `Bytes`
slices, with no zeroed allocation per record. Encode outbound records and
their size prefix into one reusable write buffer, with no `Vec` per record
and no `BytesMut` per varint. Take the session lock once per batch, not per
record.

tungstenite stays in this quest, driven from the one task (it runs over any
`AsyncRead + AsyncWrite`). Replacing it is the separate
[codec](/quest/m2/perf/stream/codec.md) quest, so the bench attributes each
change on its own.

Public API: the constructor shape changes in `qmux`, `moq-tokio`, and any
binding that starts a qmux session. The Impact bullet says how to migrate.

## Related

- [qmux byte counters](/quest/m2/wire-stats/qmux.md) - refreshes `Wire` counters on qmux's read/write path
- [qmux leases receive memory](/quest/m2/qmux-receive-lease.md) - changes qmux's `RecvStream` and window defaults
