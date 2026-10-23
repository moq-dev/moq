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
fn poll_acked(&mut self, cx: &mut Context, offset: u64) -> Poll<Result<Option<()>, Self::Error>>;
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
first, in the shape of [the driver poll waits](/quest/m2/driver-poll.md),
with an async wrapper:

```rust
impl SendStream {
    pub fn poll_acked(&mut self, cx: &mut Context<'_>, offset: u64) -> Poll<Result<(), AckedError>>;
    pub async fn acked(&mut self, offset: u64) -> Result<(), AckedError>;
}
```

`AckedError` says why the bytes will never be acknowledged: a reset, the
peer's STOP_SENDING, the connection's loss, or an offset past the data
written. Both drivers sit on `moq-quic`'s `SendStream::acked(offset)`, whose
`false` asks for a `StreamEvent::Acked { id, offset }` once the acknowledged
prefix reaches the offset. Neither driver handles that event today
(moq-tokio's catch-all arm, moq-uring's `sweep_stream`), so waking the
stream's waiter on it is the core of the work. Three traps:

- Once a stream is freed, `acked` fails with `ClosedStream`, which cannot tell
  a finished stream whose every byte was acknowledged from a reset one. The
  driver answers from how the handle saw the stream end: finished, with no
  reset or STOP_SENDING, means delivered. moq-uring's driver already records
  that end; moq-tokio's handle tracks `finished` and, since #6196, the reset.
- `moq-quic`'s `acked` panics on an offset past the data written to an
  unreset stream. The driver returns the error instead, since a panic under
  the connection lock poisons it for every handle.
- The offset counts bytes on the QUIC stream. The trait method takes a byte
  offset, like `moq-quic` (decided 2026-10-17 on #5817), and the starvation
  quest's writer counts the bytes it writes, so neither driver needs a public
  `offset()`. A layer that writes its own prefix adds its length: `quic::web`
  already tracks its WebTransport header for `reset_at`.

The moq-tokio and moq-uring adapters implement the trait method over the
driver's. Leave the browser transport on the default; the browser's `WebTransportSendStream.getStats()` is
unimplemented in shipping Chrome and its `bytesAcknowledged` is at risk in the
W3C draft. Decided 2026-10-22 (maintainer): qmux's `poll_acked` reports
unsupported too, since writing into TCP is not delivery; over qmux the
histogram stays at group granularity.

Acceptance includes the first two traps, in each driver: a finished stream
that `moq-quic` has freed (so it answers `ClosedStream`) resolves `poll_acked`
as delivered, while a reset one resolves it with the error; and an offset past
the data written returns an error, not a panic, before and after finish.

Tests also cover: offset already acknowledged before the first poll, an
offset in the middle of an in-flight frame, several waiters on one stream in
offset order, reset by sender, STOP_SENDING by the receiver, and session
close.


Decided in the 2026-09-30 audit: moved to m2 with its only consumer,
[frame-granularity starvation](/quest/m2/starvation-frames.md).

## Required

- [QUIC driver poll waits](/quest/m2/driver-poll.md) - the leak-free poll
  shape and waiter lists `poll_acked` joins

## Related

- [Starvation at frame granularity](/quest/m2/starvation-frames.md) - the
  moq-net consumer
