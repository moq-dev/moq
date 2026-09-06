# [S] web-transport-trait awaits an acknowledged stream offset

## Goal

A released `web-transport-trait` lets a sender await acknowledgment of a
named byte offset on a send stream, a released `web-transport-noq` implements
it, and every other backend reports that it cannot rather than returning a
guess.

## Plan

The work lives in `moq-dev/web-transport`. This quest exists so the release
it produces is one condition dependents wait on; it supersedes the design in
moq-dev/web-transport#368, whose snapshot-counter shape loses the wakeups a
latency sample needs.

Add one method to `SendStream`, in the trait's poll style:

```rust
fn poll_acked(&mut self, cx: &mut Context, offset: u64) -> Poll<Result<Option<Acked>, Self::Error>>;
```

It is ready with `Some` once every byte below `offset` has been acknowledged.
`Acked` carries the ACK-delay-corrected receive instant from noq. It resolves
with an error once the stream is reset by either side or the session closes,
so a waiter never hangs on bytes the peer will never acknowledge. The default
implementation is ready with `Ok(None)` immediately: `None` means the backend
cannot see acknowledgments, the same convention the trait's `Stats` methods
use. A default body cannot construct a backend's own `Self::Error`, so
unsupported has to live in the return type rather than the error, and a
consumer must treat `None` as unknown, never as delivered.

Implement it in `web-transport-noq` over the noq-proto accessor. Leave
`web-transport-quinn`, `web-transport-quiche`, and `web-transport-wasm` on
the default; the browser's `WebTransportSendStream.getStats()` is
unimplemented in shipping Chrome and its `bytesAcknowledged` is at risk in the
W3C draft. qmux over a reliable transport may treat serialization as
acknowledgment only if the qmux quest decides that is honest; until then it
also reports unsupported.

Tests cover: offset already acknowledged before the first poll, an offset in
the middle of an in-flight frame, several waiters on one stream in offset
order, a waiter whose offset lies beyond the final size, reset by sender,
STOP_SENDING by the receiver, and session close.

Cut releases of `web-transport-trait` and `web-transport-noq`. The quest
completes when both are on crates.io.

## Required

- [Per-stream ACK progress in noq](/quest/m2/quic/ack-progress.md) - the
  noq-proto accessor the adapter reads

## Related

- [Starvation at frame granularity](/quest/m2/qos/starvation-frames.md) - the
  moq-net consumer
- [qmux on the QUIC stream state machine](/quest/m2/quic/qmux.md) - decides
  what acknowledgment means over a reliable transport
