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

Implement it in the moq-tokio and moq-uring adapters over `moq-quic`'s
`SendStream::acked(offset)`, which returns whether every byte before the
offset is acknowledged and fails once it never will be. A `false` asks
`moq-quic` to report `StreamEvent::Acked { id, offset }` when the stream's
acknowledged prefix reaches it, so the adapter wakes waiters at or below that
offset; moq-uring's `sweep_stream` currently ignores the event. `acked` fails
once the stream is freed, after `StreamEvent::Finished`, which means
everything was acknowledged, so the adapter answers from that. The trait
method takes a byte offset, like `moq-quic` (decided 2026-10-17 on #5817);
the starvation quest's writer counts the bytes it writes. Leave the browser transport on the default; the browser's `WebTransportSendStream.getStats()` is
unimplemented in shipping Chrome and its `bytesAcknowledged` is at risk in the
W3C draft. qmux over a reliable transport may treat serialization as
acknowledgment only if the qmux quest decides that is honest; until then it
also reports unsupported.

Tests cover: offset already acknowledged before the first poll, an offset in
the middle of an in-flight frame, several waiters on one stream in offset
order, a waiter whose offset lies beyond the final size, reset by sender,
STOP_SENDING by the receiver, and session close.


Decided in the 2026-09-30 audit: moved to m2 with its only consumer,
[frame-granularity starvation](/quest/m2/starvation-frames.md).

## Related

- [Starvation at frame granularity](/quest/m2/starvation-frames.md) - the
  moq-net consumer
- [qmux on the QUIC stream state machine](/quest/m1/quic-qmux.md) - decides
  what acknowledgment means over a reliable transport
