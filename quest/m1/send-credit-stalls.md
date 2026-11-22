# [M] Session stats count send-credit stalls

## Goal

A connection whose sender is blocked on the peer's connection-level flow
control (`MAX_DATA`) shows it in `moq_net::session::Stats` and in the relay's
session stats. The count covers time spent blocked and the number of blocked
episodes, for native QUIC and Rust QMux sessions. An operator can then tell
"the peer gave us no credit" apart from loss, congestion, or a stuck
application, the way `streams_evicted` already shows a receive window too
small for what the peer sends.

Non-goals: per-stream credit stalls; a warning log, since a warning needs an
arbitrary threshold while a counter shows up on the existing dashboards; and
the JS WebSocket fallback, which [its own quest](/quest/m3/send-credit-stalls-js.md)
owns.

## Plan

Decided 2026-11-14 (maintainer, /quest-plan):

- ✅ A counter in session stats (rejected: a warning log after N seconds, which
  needs a constant; both together).
- ✅ `main` only (rejected: backport to `release`). It adds API, and on the
  release line [the receive-credit fix](https://github.com/moq-dev/moq/pull/8808)
  removes the cause.

Decided 2026-11-21 (maintainer, /quest-plan of the main → release
flow-control review):

- ✅ m1, and the [release cut](/quest/m3/release/cut.md) waits on it
  (rejected: m1 without the cut waiting; staying in m3). The cut ships the
  flow-control rework (smaller windows, leased credit, fastest-reader pacing,
  paused allowances), whose failure mode is a hang, so it ships with a way to
  see one. moq.pro's live fleet takes the cut.
- ✅ Rust only; the JS WebSocket half moved to
  [its own quest](/quest/m3/send-credit-stalls-js.md) (rejected: one quest for
  both). Operators read relay-side stats, and the JS half starts from zero.

Today, a received DATA_BLOCKED is only logged at trace
(`rs/moq-quic/src/connection/mux/mod.rs`, and `connection/mod.rs` for QUIC).
`moq-quic`'s `FrameStats.data_blocked` counts the DATA_BLOCKED frames we send,
and none of it reaches `moq-net`. Count on the sending side: an episode starts
when a write is blocked and `data_sent` equals `max_data`, the condition that
queues our DATA_BLOCKED in `connection/streams/send.rs`. A zero write limit from
our own send window is not a credit stall, and accepting the last credited byte
starts no episode until a later write blocks. Don't count received DATA_BLOCKED
frames, since a peer need not send them.

Reuse the stats plumbing [wire stats](/quest/m3/wire-stats/README.md) uses for
QMux sessions. Don't build another registry. This quest isn't one of that
epic's children: its goal is byte counts.
