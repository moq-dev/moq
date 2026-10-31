# [S] Certificate reload and qmux's tasks are supervised

## Goal

A panic in moq-tokio's certificate reload watcher fails the listener loudly instead of silently stopping certificate rotation, and a panic in one of qmux's own session or WebSocket tasks is logged and counted like moq-tokio's per-connection tasks.

## Plan

Left over from [moq-dev/moq#7360](https://github.com/moq-dev/moq/pull/7360), which supervises the other long-lived tasks.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run).

- **Reload watcher** (`rs/moq-tokio/src/tls.rs`, `ReloadState`/`Reloading`): run it under the same supervision #7360 gives a dead QUIC endpoint, so its panic surfaces as an error from `Listener::accept`.
- **qmux** spawns with plain `tokio::spawn` (`rs/qmux/src/session.rs:486-487`, `rs/qmux/src/transport.rs:188,985,1195,1223,1244`) and can't reach `moq_tokio::panic`. Give qmux a small isolation hook of its own (or take one from its caller) so a panic is caught, logged, and counted, and the session closes with an error rather than hanging.

Public API: none expected; if qmux needs a hook in its public API, bring the shape to the maintainer. Wire: none.

## Required

- [Task supervision](/quest/m2/task-supervision.md) - its PR, #7360, adds the supervisor and `panic::isolate` this extends
