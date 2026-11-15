# [M] Relay stream listeners on the workers

## Goal

The relay serves its TCP listeners (WebSocket sessions, qmux over TCP, and
the HTTP routes) from the io_uring workers rather than the shared tokio
runtime. A qmux media session over TCP is then pinned to one thread for its
whole life, exactly like a QUIC one. A `unix` listener stays on the shared
tokio runtime.

## Plan

`uring::Workers::bind` (`rs/moq-relay/src/uring.rs`) serves QUIC only. The
relay serves `listen`'s `tcp`/`unix` listeners beside it from the shared tokio
runtime, through `init_streams` (`rs/moq-relay/src/relay.rs`). Move the TCP
listeners onto the workers: each worker binds its own listener in the
reuseport group and runs the router from
[ring TCP](/quest/m9/stream-transports/ring.md) on it. A Unix socket has no reuseport
group, so a `unix` listener keeps its one shared acceptor; the integration
test covers it too.

The split of work stays what the `uring.rs` module docs already describe: the worker
owns everything transport-shaped, while authentication and session
supervision run on the shared tokio runtime that owns the HTTP client, the
timers, and the origins. A qmux session handle is `Send + Sync` however its
transport is driven, which is what makes that handoff free here too.

moq-tokio spawns each qmux `Driver` on its own runtime (`spawn_qmux` in
`rs/moq-tokio/src/lib.rs`), so its `tcp::Listener::accept`,
`websocket::Listener::accept`, and `websocket::accept` return only the session.
A worker must run the driver on its own local task set instead, so
`moq_tokio::websocket::accept` must hand the driver back; the qmux
constructors already return it (decided 2026-11-01, maintainer, in
[moq-dev/moq#7494](https://github.com/moq-dev/moq/pull/7494)).

Decided 2026-11-15 (maintainer, /quest-complete): ✅ reshape
`moq_tokio::websocket::accept` itself to return the session and its driver
(rejected: an additive sibling beside it). That breaks moq-tokio's published
API, which the lab never touches, so ✅ the reshape is its own m2 quest,
[WebSocket accept hands back the driver](/quest/m2/websocket-accept-driver.md),
listed in Required (rejected: landing it here with the rust2 migration).
This quest keeps only the relay's internal half.

Keep the ops and web listeners' behavior identical: the same routes, the same
CORS scoping, the same landing-page fallback, and the same
`/certificate.sha256` fingerprint. Extend `rs/moq-relay/tests/integration/runtime_uring.rs`
to prove a WebSocket session and an HTTP route both work when served from a
worker.

## Required

- [WebSocket accept hands back the driver](/quest/m2/websocket-accept-driver.md) - the moq-tokio API that returns the qmux driver for a worker to run
- [Ring TCP](/quest/m9/stream-transports/ring.md) - the module and adapters this
  serves from
