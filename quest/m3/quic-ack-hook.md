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

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs
and remaining quests): no `Option<()>` outcome, and one offset type on both
layers. Add an offset type and two methods to `SendStream`, in the trait's
poll style:

```rust
type Offset: Clone;
fn offset(&self) -> Self::Offset;
fn poll_acked(&self, waiter: &kio::Waiter, offset: Self::Offset) -> Poll<Result<Acked, Self::Error>>;

#[non_exhaustive]
pub enum Acked { Delivered, Unsupported }
```

It is ready with `Delivered` once every byte below `offset` has been acknowledged.
It carries no instant: the caller records one when woken, which includes the
peer's delayed-ACK timer (up to `max_ack_delay`) as well as the return trip.
Decided 2026-10-17 on #5817, which dropped `moq-quic`'s ACK-delay-corrected
instant until a consumer shows it matters. It resolves
with an error once the stream is reset by either side or the session closes,
so a waiter never hangs on bytes the peer will never acknowledge. The default
implementation is ready with `Ok(Acked::Unsupported)` immediately. A default
body cannot construct a backend's own `Self::Error`, so unsupported has to
live in the return type rather than the error, and a consumer must treat
`Unsupported` as unknown, never as delivered.

Each QUIC driver, moq-tokio's `quic::driver` and moq-uring's, gets the hook
first, in the drivers' caller-owned waiter shape,
with an async wrapper:

```rust
impl send::Stream {
    pub fn poll_acked(&self, waiter: &kio::Waiter, offset: send::Offset) -> Poll<Result<(), send::Error>>;
    pub async fn acked(&self, offset: send::Offset) -> Result<(), send::Error>;
}
```

The same pair goes on `send::Finished`, the handle `finish(self)` returns
([moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950)), and the methods take its opaque `send::Offset`, which only
`offset()` mints, instead of a `u64`. The `send::Error` says why the bytes will
never be acknowledged: a reset, the peer's STOP_SENDING, or the connection's
loss. Both drivers forward to `moq-quic`'s own `send::Stream::poll_acked`
wait, which replaced `StreamEvent::Acked`. That wait is `#[cfg(test)]` and
`pub(crate)` today (`rs/moq-quic/src/connection/streams/send.rs`), so the
first step makes it public in a normal build. Four traps, each settled in moq-quic where the wait
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
- The offset counts bytes on the QUIC stream, but nobody computes one: the
  trait's `Offset` is the driver's minted `send::Offset`, and the lag
  histogram's writer records `offset()` after each frame end instead of
  counting the bytes it writes. A layer that writes its own prefix mints
  offsets that already include it, so `quic::web` forwards the driver's
  `Offset` and its WebTransport header needs no adjustment. A backend that
  cannot see acknowledgments uses `type Offset = ()` and keeps the default
  `poll_acked`.

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
while a reset one resolves it with the error. An offset past the data
written can only come from another stream, since only `offset()` mints one,
so that case is the foreign-offset trap: build it from a longer stream and
check it panics before the connection lock, leaving other handles on the
connection usable, before and after finish.

Tests also cover: offset already acknowledged before the first poll, an
offset in the middle of an in-flight frame, several waiters on one stream in
offset order, reset by sender, STOP_SENDING by the receiver, and session
close.

Decided in the 2026-09-30 audit: moved to m2 with its consumers, the
[viewer lag histogram](/quest/m3/qos/lag-histogram.md) and a
delivery-delay histogram, whose plan was deleted on 2026-11-09.

Split 2026-11-12 (maintainer, /quest-audit before the cut): the trait shape
(`Offset`, `offset()`, `Acked`, and a defaulted `poll_acked`) landed in m2,
because `poll::SendStream` is public and unsealed and the cut would freeze it.
As landed, `Offset` is `Clone` rather than `Copy`, since moq-net's boxed
stream erases it behind an `Arc`, and a backend without acknowledgments uses
`type Offset = ()` rather than a byte count. Every in-tree backend, including
moq-tokio's and moq-uring's `quic::web`, is on `()` with the default. This
quest keeps the driver half: moq-quic's `poll_acked` leaves `#[cfg(test)]`,
and the moq-tokio and moq-uring adapters switch `Offset` to the driver's
`send::Offset` and implement the hook.

