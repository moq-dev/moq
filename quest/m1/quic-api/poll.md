# [S] The QUIC drivers' waits poll

## Goal

The stream, datagram, close, and accept waits in `moq_tokio::quic::driver`
and moq-uring's driver have public `poll_*` forms taking `&self,
&kio::Waiter`: `Connection::poll_open_uni`, `poll_open_bi`,
`poll_accept_uni`, `poll_accept_bi`, `poll_recv_datagram` and `poll_closed`,
`send::Stream::poll_closed` (today's `stopped`), and `Endpoint::poll_accept`.
`send::Stream::poll_acked`, which [poll_acked](/quest/m2/quic-ack-hook.md)
adds, takes the same shape. The async methods are thin `kio::wait` wrappers,
and `send::Finished` implements `Future`, resolving when every byte is
acknowledged or the peer stops the stream. No `tokio::sync::Notify` is left
in the driver.

## Plan

Moved to m1 into the [moq-quic API](/quest/m1/quic-api/README.md) epic
(maintainer, 2026-10-23): an async-only driver is quinn's shape, and it forces
moq-tokio's async-to-poll adapter (a boxed future per operation, a copy per
write, deferred finish and reset, an emulated `closed()`). Poll forms are the
primary surface. [Native WebTransport](/quest/m1/quic-api/native-web.md) and
[poll_acked](/quest/m2/quic-ack-hook.md) both need them.

Decided 2026-10-25 (maintainer), replacing the 2026-10-23 shape:

- **moq-quic owns the waits** ([waits](/quest/m1/quic-api/waits.md)). Each
  connection and stream form here takes the lock and forwards to moq-quic's
  `poll_*` in one line; the driver keeps no waiter table of its own. Only
  `Endpoint::poll_accept` waits on driver state, the queue of handshaken
  connections, and parks on a `kio::WaiterList` there.
- **`&self, &kio::Waiter`**, the `rs/AGENTS.md` convention, instead of
  `&mut self, cx` with a `kio::Park` in each handle. The earlier objection,
  that kio would enter the driver's public API so a kio break is a driver
  break, no longer holds: moq-net already re-exports kio, and moq-quic takes
  `&kio::Waiter` too. The caller owns the waiter, so a poll that is
  abandoned leaves nothing behind, and handles stay `&self`. moq-net's
  `transport::poll` traits take the same shape, so their native impls
  forward without a `Park`.

```rust
impl Connection {
    pub fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<recv::Stream, connection::Error>>;
    pub fn poll_open_uni(&self, waiter: &kio::Waiter, options: send::Options<'_>) -> Poll<Result<send::Stream, connection::Error>>;
    pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<connection::Error>;
}
impl send::Stream {
    pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<Option<VarInt>, send::Error>>;
}
```

Keep the existing guarantees, such as draining accepted streams and
datagrams from a closed connection before its error (moq-quic's waits carry
that now). The `Connecting` handshake, `Endpoint::wait_idle`, and
`Endpoint::wait_all_draining` move off `Notify` too, but get no poll form
until a caller needs one. Report the public API:
new poll methods on `Connection`, `send::Stream` and `Endpoint`, with the
async forms unchanged.

## Required

- [Waits](/quest/m1/quic-api/waits.md) - moq-quic's `poll_*` forms these forward to

## Related

- [Native WebTransport](/quest/m1/quic-api/native-web.md) - its native QUIC `poll::Session` calls these
- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - adds `send::Stream::poll_acked` in this shape
