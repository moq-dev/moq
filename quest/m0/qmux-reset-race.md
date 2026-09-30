# [XS] qmux handles RESET_STREAM under one lock

## Goal

qmux's RESET_STREAM handling never panics when the writer task removes the
receive stream concurrently. Today `session.rs` checks
`streams.recv.contains_key`, drops the lock, and later re-locks with
`.expect("live recv stream")`; a `StopSending` queued by a dropped
`RecvStream` can remove the entry in between, and `panic = "abort"` ends the
process. The WebSocket fallback reaches it.

## Plan

- In moq-dev/web-transport `rs/qmux/src/session.rs`, look the entry up once
  under a single lock (`if let Some(recv) = get_mut`); a missing entry means
  the stream already closed and the reset is a no-op.
- Regression test that removes the entry between the two points.
- Audit the other `expect`s on the stream maps (`sched.rs`) for the same
  pattern.
- Release qmux and bump it here.

Public API: none. Wire: none.

## Related

- [qmux on noq-proto](/quest/m1/quic/qmux.md) - replaces these stream maps entirely, later
