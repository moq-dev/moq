# [M] One relay serve path for every runtime

## Goal

The relay serves a session through one `Connection::serve` on every
runtime, and one `listen::Config` to `moq_net::Server` mapping in moq-tokio
serves the tokio, WebSocket, and io_uring listeners, so a lifecycle or
config fix lands once instead of drifting between runtimes.

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. Internal only.

`uring::serve_request` (`rs/moq-relay/src/uring.rs:803-926`) re-implements
`Connection::serve` (`connection.rs:90-164`) step for step (admit, QUIC
limits, stats, limits, origins, `preparing`, `serve()`, a drain-bounded
`ok()`, spawn the driver, supervise), and `Workers::bind` re-implements the
config mapping also found at `rs/moq-tokio/src/server.rs:424` and
`websocket.rs:66`. Fixes patching exactly this gap: 58eaa6afd5
(listen.extensions), 8a13d58910 (MTU), 6bd1306d6e (SETUP tokens),
1f56330d21 (handshake deadline), d17b84ee7f (drain across handshakes),
4576bb1231 (admission bound), c11192c7e6, 67df70012d.
`Workers::bind` now destructures `moq_tokio::listen::Resolved` (from
`listen::Config::resolve`) exhaustively, so a new listen field fails to
compile there; the tokio sites still read `listen::Config` directly.

Implement the sealed `incoming::Sealed` for a uring `Request` whose
`Driver` runs on the worker handle, with admission hopping to the shared
runtime through `Lifecycle::shared`, so one `Connection::serve` serves
every runtime. Use that one config mapping at all three sites. Do this before the io_uring packaging work publishes the
duplicated path.

Public API: none. Wire: none.

## Related

- [io_uring relay package](/quest/m2/relay-io-uring-package.md) - ships the path this unifies
