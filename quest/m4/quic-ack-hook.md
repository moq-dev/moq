# [S] The QUIC drivers' acknowledgment wait is tested at its traps

## Goal

moq-tokio's and moq-uring's QUIC drivers each have tests covering the traps
of their crate-private acknowledgment wait, the one their `quic::web`
adapters answer `poll_acked` with. The wait stays crate-private.

## Plan

moq-net's half landed: `transport::poll::SendStream` carries `Offset`,
`offset()`, and `poll_acked` as its only acknowledgment primitive, and the
moq-tokio and moq-uring `quic::web` adapters implement it over crate-private
driver methods ([api-ack-one-primitive](https://github.com/moq-dev/moq/pull/8845)).
Those methods, `poll_acked` on `send::Stream` and `send::Finished` and
`Finished::offset`, forward to `moq-quic`'s public `send::Stream::poll_acked`.
They fail with the peer's stop code (`send::Error::Stopped`) or the
connection's loss (`send::Error::Connection`); a reset consumes the handle, so
no handle ever waits past one. qmux and the browser transport stay on the
trait's default `Acked::Unsupported` (decided 2026-10-22 and 2026-10-26,
maintainer): writing into TCP is not delivery, and the browser's
`WebTransportSendStream.getStats()` is unimplemented in shipping Chrome.

Decided 2026-11-15 (maintainer, /quest-complete): ✅ keep only the trap tests,
on the crate-private methods, and drop the public driver API this quest
planned (`poll_acked`, `acked`, and `offset()` on `send::Stream` and
`send::Finished`) (rejected: make the wait public; delete the quest). Since
[moq-dev/moq#8845](https://github.com/moq-dev/moq/pull/8845) the adapters
answer `poll_acked`, so nothing needs the drivers' wait public. Moved from m3
to m4 the same day (maintainer): it is now tests only (rejected: keep it in m3).

Traps to test through each driver:

- A finished stream whose every byte was acknowledged resolves as delivered.
  moq-quic answers from the send-stream end it keeps until the id is
  released: finished, with no STOP_SENDING, means delivered.
- An offset past the data written cannot be built, since only `offset()`
  mints one, so it can only come from another stream: build it from a longer
  stream and check it panics before the connection lock (moq-tokio's
  `Half::poll_acked` checks it first), leaving other handles on the
  connection usable, before and after finish.
- `reset_at` below the acknowledged prefix, or STOP_SENDING after a reset
  (which truncates to 0), shrinks moq-quic's acknowledged prefix; keep the
  prefix a wait reports monotonic.

Also cover an offset acknowledged before the first poll, an offset in the
middle of an in-flight write, several waiters on one stream in offset order,
STOP_SENDING by the receiver, and the connection's loss.

The wait carries no instant: a caller records one when woken, which includes
the peer's delayed-ACK timer (up to `max_ack_delay`) as well as the return
trip (decided 2026-10-17 on #5817).

Public API: none. Wire: none.

## Related

- [Deadline-aware delivery](/quest/m4/quic-deadline.md) - the same pattern
  for a moq-net method backed by `moq-quic`
