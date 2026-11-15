# [S] The QUIC drivers' acknowledgment wait is public and tested at its traps

## Goal

moq-tokio's and moq-uring's QUIC drivers let a caller await acknowledgment
of a minted `send::Offset` on `send::Stream` and `send::Finished`, publicly,
and each driver's tests cover the traps that wait has.

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

What is left is making them public, in the drivers' caller-owned waiter shape
with an async wrapper:

```rust
impl send::Stream {
    pub fn poll_acked(&self, waiter: &kio::Waiter, offset: send::Offset) -> Poll<Result<(), send::Error>>;
    pub async fn acked(&self, offset: send::Offset) -> Result<(), send::Error>;
}
```

The same pair, and `offset()`, go on `send::Finished`.

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

## Waiting

- Maintainer: whether the drivers' wait goes public at all. Nothing in the
  tree needs it public since moq-net reaches it through the adapters, and the
  rules keep an API private until a consumer needs it. Options: (1) keep the
  quest, if an out-of-tree driver user is expected to await
  acknowledgments; (2) recommended: drop the public half and keep only the
  trap tests, on the crate-private methods, resizing this quest to XS.

## Related

- [Deadline-aware delivery](/quest/m4/quic-deadline.md) - the same pattern
  for a moq-net method backed by `moq-quic`
