# [M] Relay stream listeners on the workers

## Goal

The relay serves its WebSocket sessions, and its HTTP routes, from the
io_uring workers rather than the shared tokio runtime. A qmux media session is
then pinned to one thread for its whole life, exactly like a QUIC one.

## Plan

`uring::Workers::bind` currently refuses `listen`'s `tcp`/`unix` listeners and
tells the operator to serve them from a separate tokio server. Replace that
refusal with real support: each worker binds its own listener in the reuseport
group and runs the router from
[stream](/quest/m1/uring-tcp/stream.md) on it.

The split of work stays what `uring.rs` already documents: the worker owns
everything transport-shaped, while authentication and session supervision run
on the shared tokio runtime that owns the HTTP client, the timers, and the
origins. A qmux session handle is `Send + Sync` however its transport is
driven, which is what makes that handoff free here too.

Keep the ops and web listeners' behavior identical: the same routes, the same
CORS scoping, the same landing-page fallback, and the same
`/certificate.sha256` fingerprint. Extend `tests/runtime_uring.rs` to prove a
WebSocket session and an HTTP route both work when served from a worker.

## Required

- [Stream](/quest/m1/uring-tcp/stream.md) - the module and adapters this
  serves from
