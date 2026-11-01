# [S] moq-net's send stream awaits an acknowledged offset

## Goal

moq-net's own `transport::poll::SendStream` lets a sender await
acknowledgment of a named byte offset on a send stream, the moq-tokio and
moq-uring adapters implement it over `moq-quic`, and every other backend
reports that it cannot rather than returning a guess.

## Plan

Decided in the 2026-10-05 audit: re-planned on moq-net's own transport
traits. moq-net has no `web-transport-trait` dependency, so it could not
call a hook that exists only upstream. The method lives on
`rs/moq-net/src/transport.rs`'s `poll::SendStream`, implemented by the
moq-tokio and moq-uring adapters (moq-tokio's through `quic::web`); no
`web-transport-trait` release is needed. Rejected: keeping the method
upstream. This quest supersedes the design in moq-dev/web-transport#368,
whose snapshot-counter shape loses the
wakeups a latency sample needs.

Add one method to `SendStream`, in the trait's poll style:

```rust
fn poll_acked(&self, waiter: &kio::Waiter, offset: u64) -> Poll<Result<Option<()>, Self::Error>>;
```

It is ready with `Some` once every byte below `offset` has been acknowledged.
It carries no instant: the caller records one when woken, which includes the
peer's delayed-ACK timer (up to `max_ack_delay`) as well as the return trip.
Decided 2026-10-17 on #5817, which dropped `moq-quic`'s ACK-delay-corrected
instant until a consumer shows it matters. It resolves
with an error once the stream is reset by either side or the session closes,
so a waiter never hangs on bytes the peer will never acknowledge. The default
implementation is ready with `Ok(None)` immediately: `None` means the backend
cannot see acknowledgments, the same convention the trait's `Stats` methods
use. A default body cannot construct a backend's own `Self::Error`, so
unsupported has to live in the return type rather than the error, and a
consumer must treat `None` as unknown, never as delivered.

Each QUIC driver, moq-tokio's `quic::driver` and moq-uring's, gets the hook
first, in the shape of [the driver poll waits](/quest/m1/quic-api/poll.md),
with an async wrapper:

```rust
impl send::Stream {
    pub fn poll_acked(&self, waiter: &kio::Waiter, offset: u64) -> Poll<Result<(), send::Error>>;
    pub async fn acked(&self, offset: u64) -> Result<(), send::Error>;
}
```

The same pair goes on `send::Finished`, the handle `finish(self)` returns
([moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950)), and the methods take its opaque `send::Offset`, which only
`offset()` mints, instead of a `u64`. The `send::Error` says why the bytes will
never be acknowledged: a reset, the peer's STOP_SENDING, or the connection's
loss. Both drivers forward to `moq-quic`'s own `poll_acked` wait, which
[waits](/quest/m1/quic-api/waits.md) makes of today's `acked(offset)` and
`StreamEvent::Acked`. Four traps, each settled in moq-quic where the wait
lives:

- A finished stream whose every byte was acknowledged must not look like a
  reset one. moq-quic answers from the queryable send-stream end
  it keeps until the id is released:
  finished, with no reset or STOP_SENDING, means delivered.
- An offset past the data written cannot be built, and a foreign one panics
  before moq-tokio takes the connection lock, since a panic under the lock
  poisons it for every handle.
- `reset_at` below the acknowledged prefix, or STOP_SENDING after a reset
  (which truncates to 0), shrinks the acknowledged prefix. The waiters fail
  anyway; keep the prefix a `poll_acked` reports monotonic.
- The offset counts bytes on the QUIC stream. The trait method takes a byte
  offset, like `moq-quic` (decided 2026-10-17 on #5817), and the lag
  histogram's writer counts the bytes it writes, so neither driver needs a public
  `offset()`. A layer that writes its own prefix adds its length: `quic::web`
  already tracks its WebTransport header for `reset_at`.

The moq-tokio and moq-uring adapters implement the trait method over the
driver's. Leave the browser transport on the default; the browser's `WebTransportSendStream.getStats()` is
unimplemented in shipping Chrome and its `bytesAcknowledged` is at risk in the
W3C draft. Decided 2026-10-22 (maintainer): qmux's `poll_acked` reports
unsupported too, since writing into TCP is not delivery. Decided 2026-10-26
(maintainer, on [moq-dev/moq#6791](https://github.com/moq-dev/moq/pull/6791)):
over qmux and the browser transport, the lag histogram, `dropped`, and the
delivery-delay histogram are absent, never zero; group granularity is
rejected as a fallback.

Acceptance includes the first two traps, through each driver: a finished
stream whose bytes were all acknowledged resolves `poll_acked` as delivered,
while a reset one resolves it with the error; and an offset past the data
written returns an error, not a panic, before and after finish.

Tests also cover: offset already acknowledged before the first poll, an
offset in the middle of an in-flight frame, several waiters on one stream in
offset order, reset by sender, STOP_SENDING by the receiver, and session
close.


Decided in the 2026-09-30 audit: moved to m2 with its consumers, the
[viewer lag histogram](/quest/m2/qos/lag-histogram.md) and the
[delivery-delay histogram](/quest/m2/delivery-delay.md).

## Required

- [QUIC driver poll waits](/quest/m1/quic-api/poll.md) - the `&self, &kio::Waiter`
  driver shape `poll_acked` takes
