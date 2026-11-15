# [M] Session stats count send-credit stalls

## Goal

A connection whose sender is blocked on the peer's connection-level flow
control (`MAX_DATA`) shows it in `moq-net`'s transport `Metrics` and in the
relay's session stats. The count covers time spent blocked and the number of
blocked episodes, for QMux and native QUIC sessions, and for the JS WebSocket
fallback where `js/net` reports stats. An operator can then tell "the peer gave
us no credit" apart from loss, congestion, or a stuck application.

Non-goals: per-stream credit stalls, and a warning log. A warning needs an
arbitrary threshold, while a counter shows up on the existing dashboards.

## Plan

Decided 2026-11-14 (maintainer, /quest-plan):

- ✅ A counter in session stats (rejected: a warning log after N seconds, which
  needs a constant; both together).
- ✅ `main` only (rejected: backport to `release`). It adds API, and on the
  release line [the receive-credit fix](/quest/m0/release-qmux-credit.md)
  removes the cause.

Today, a received DATA_BLOCKED is only logged at trace
(`rs/moq-quic/src/connection/mux/mod.rs`, and `connection/mod.rs` for QUIC).
`moq-quic`'s `FrameStats.data_blocked` counts the DATA_BLOCKED frames we send,
and none of it reaches `moq-net`. Count on the sending side, when data is
queued but connection credit is zero. Don't count received DATA_BLOCKED frames,
since a peer need not send them. `js/qmux` ignores the blocked frames, and
`js/net`'s WebSocket stats report nothing, so the JS side of this change starts
from zero.

Reuse the stats plumbing [wire stats](/quest/m3/wire-stats/README.md) uses for
QMux sessions. Don't build another registry. This quest isn't one of that
epic's children: its goal is byte counts.

m3 because an operator waits on it: on a release relay the leak stayed
invisible for days.
