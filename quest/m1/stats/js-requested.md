# [S] @moq/net serves a broadcast's tracks on request

## Goal

A JS publisher can answer a subscription for a track it has not created:
`@moq/net` exposes the produced broadcast's request queue publicly, mirroring
Rust's `broadcast.dynamic()`, `requested_track()`, `accept` and `reject`, with
matching names. The `.echo` broadcast needs it, because a publisher can
subscribe before the viewer knows its track name.

## Plan

- `js/net/src/broadcast.ts` already queues unknown-name subscriptions in its
  `requested` signal and drains them through the private `#requested()`,
  which only the wire layer reads. Expose a public handle that yields each
  request by priority, with `accept` returning the track producer and
  `reject` refusing it, and keep the wire layer on the same path so there is
  one queue.
- Match Rust's lifetime rule: dropping the last handle rejects its queued
  requests (`Dynamic::drop` in `rs/moq-net/src/model/broadcast.rs`), and a
  broadcast that never takes a handle treats unknown names exactly as today.
- Additive on main: a new export on `@moq/net`. Document it in `doc/lib/js`.
- Tests: an unknown-name subscription reaches the handle and is served after
  `accept`; `reject` refuses it; a broadcast without the handle refuses as
  before.
