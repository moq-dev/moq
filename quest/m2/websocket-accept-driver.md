# [S] moq-tokio's WebSocket accept hands back the qmux driver

## Goal

`moq_tokio::websocket::accept` returns the qmux driver to its caller instead
of spawning it on the tokio runtime, so a caller such as an io_uring worker
runs the driver on its own task set. Every existing caller (moq-relay's
`websocket.rs` and moq-tokio's own server tests) spawns the driver itself and
behaves as today.

## Plan

Decided 2026-11-15 (maintainer, /quest-complete): ✅ reshape
`websocket::accept` itself to return the session and its driver, as its own
m2 quest that [relay stream listeners](/quest/m9/stream-transports/relay.md)
requires (rejected: landing it with the rust2 migration; an additive sibling
beside `accept`).

Today `accept` builds the qmux session, hands it to `spawn_qmux`
(`rs/moq-tokio/src/lib.rs`), which spawns the `Driver` on the current
runtime, and then reads SETUP into a `server::Request`
(`rs/moq-tokio/src/websocket.rs`). The SETUP read needs the driver running,
so the driver cannot come back only at the end: return it once QMux is
established, beside whatever finishes the SETUP read and admission, and keep
the accept deadline covering both.

This breaks moq-tokio's published API, so the PR's title takes `!` and its
Impact section carries the migration note: a caller that used the old
`accept` spawns the returned driver itself.

Public API: breaking, `moq_tokio::websocket::accept`'s return type. Wire: none.
