# [L] Streams that cannot be misused

## Goal

Streams and datagrams in moq-quic, `moq_tokio::quic::driver`, and
moq-uring's `quic` module share one namespaced shape: `send::{Stream,
Finished, Group, Options, Error}`, `recv::{Stream, Lease, Error}`, and
`stream::{Id, Dir, Event}`, with the same signatures in both drivers. One
error type per direction replaces quinn's zoo, a write after `finish` or any
use after `reset`/`stop` does not compile, writes and reads take `Buf`
and `BufMut`, and a receive lease holds no connection lock.

## Plan

Decided 2026-10-23 (maintainer):

**Names.** No `SendXxx`, `RecvXxx`, or `XxxError` at a crate root.
`SendStream` is `send::Stream`, `SendGroup` is `send::Group`, `SendGroups`
is `send::Groups`, `SendOptions` is `send::Options`, `RecvStream` is
`recv::Stream`, the driver's `Lease` and moq-quic's `Leased` are
`recv::Lease`, and `StreamId` is `stream::Id`. `StreamEvent` keeps its name:
[waits](/quest/m1/quic-api/waits.md) deletes it.
`recv::Stream::set_priority` keeps its name, though it ranks eviction rather
than scheduling (maintainer: "priority" means "more important" both ways).
`send::Options` loses its generic group parameter; each layer defines its
own.

**Errors.** `send::Error` covers streams and datagrams: `Stopped(code)`,
`TooLarge`, `Unsupported` (peer or local datagram support off),
`Connection(connection::Error)`. `recv::Error` covers
reads and leases: `Reset(code)`, `Evicted`, `FinishedEarly(n)`, `TooLong`,
`TooLarge` (a lease past the window), `Connection`. Neither has a 0-RTT variant. Both
`#[non_exhaustive]`, both convert to `io::Error`. moq-quic mirrors the names
and adds `Blocked`, plus `Closed` for a stale `stream::Id` (its handles are
borrowed views by id, so it cannot consume). This replaces `ReadError`,
`ReadExactError`, `ReadToEndError`, `ResetError`, `WriteError`,
`StoppedError`, `FinishError`, `ReadableError`, `LeaseError`,
`SendDatagramError` (two different types at two layers today), and
`ClosedStream`. `LeaseError::Blocked(ShouldTransmit)` stops carrying a
transmit signal inside an error.

**Typestate.** In the drivers, `send::Stream::finish(self)` returns
`send::Finished`, which keeps what still applies to a finished stream:
`reset(self, ..)`, `reset_at(self, ..)`, `stopped()`, priority and group,
and acknowledgment. `reset` and `reset_at` take `self` on both types;
`recv::Stream::stop(self, code)`. Dropping an unfinished `send::Stream`
still resets it. The maintainer chose this over `finish(&mut self)` because
a reset after finish stays possible while a write after it cannot compile.
Setters take `&mut self` and return nothing in both drivers (today
moq-tokio's take `&self` and return `Result<(), ClosedStream>`).

**Offsets.** `reset_at` and `acked` given an offset past the written data
return an error instead of panicking (maintainer). First explore an opaque
`send::Offset` minted only by `offset()`, so a past-the-end offset cannot be
built; if it is tagged to its stream, a foreign one is the only error left.

**Buffers.** `Bytes` and `BytesMut` implement `Buf::copy_to_bytes` as a
zero-copy split, so one generic write covers owned and borrowed data. Send:
`write(&mut impl Buf) -> usize`, `write_all(impl Buf)`, `AsyncWrite`. Recv:
`read(&mut impl BufMut)`, `read_exact`, `read_chunk(max) -> Bytes`,
`read_to_end(limit)`, `lease`, `AsyncRead`. `write_chunk`, `write_chunks`, and
`write_all_chunks` go (`read_chunks` already went in
[moq-dev/moq#6809](https://github.com/moq-dev/moq/pull/6809)); moq-quic's `write` and
`write_chunks` become one `write(&mut impl Buf)`. Confirm on
`benches/streams.rs` that owned writes stay zero-copy. A buffer pool is out
of scope: [QUIC I/O boundary](/quest/m2/quic-io-boundary.md) owns it after a
profile.

**Leases hold no lock** (maintainer, 2026-10-23). The driver's `Lease` wraps
the connection's `MutexGuard`, so while moq-net decodes a message from a
lease, the connection's packet processing, transmits, timers, and every other
stream wait on it. `recv::Lease` instead holds owned `Bytes` chunks, taken
under a short lock, and returns their credit under another short lock on
release or drop. A lease already copies its chunks out of the receive batch
as it fills, so owning them adds no copy. A lease stays `!Send` only if
something still needs it; otherwise it may cross an await. Test that the driver keeps sending and receiving on
other streams while a lease is held.

**Datagrams.** `send_datagram(impl Buf, send::Options)`, so a caller
chains its own header (WebTransport's session id) instead of allocating and
copying each datagram.

**Sans-IO state** (maintainer, 2026-10-23). moq-uring's driver and qmux each
keep a `sends: FxHashMap<StreamId, Option<End>>` because `Finished` and
`Stopped` are fire-once events, and both hand-roll a `write_limit()` probe,
`copy_to_bytes`, and `debug_assert` around `write`. moq-quic keeps a
queryable send-stream end (finished and acknowledged, stopped with a code,
reset) until the id is released, and takes `write(&mut impl Buf)`; both
consumers' copies go. The per-stream waker tables move into moq-quic in
[waits](/quest/m1/quic-api/waits.md) (maintainer, 2026-10-25, replacing "stay
in the drivers").

**Consumers' error plumbing.** moq-tokio's `quic::web` error module rewraps
`WriteError`, `ReadError`, `ReadExactError`, `ReadToEndError`, and
`ClosedStream` one for one, with per-method `map_err`s, an `unreachable!`
for unordered reads, and a four-level match in `session.rs`; collapse it onto
`send::Error` and `recv::Error`.

**Acked.** The driver's waiter method arrives with
[poll_acked](/quest/m2/quic-ack-hook.md). Decided 2026-10-27 (maintainer):
the drivers do not match `StreamEvent::Acked` explicitly here, since
[waits](/quest/m1/quic-api/waits.md) deletes `StreamEvent` right after, so it
would be throwaway work.

**moq-uring.** Its driver gets the same names and signatures and passes
`send::Options` to the open, as moq-tokio's does, instead of applying them
through setters afterwards. Moved here from drop web-transport-trait.

Update every consumer (`quic::web` in both crates, qmux, the backends,
moq-relay's tests). `poll_*` forms come in [poll](/quest/m1/quic-api/poll.md). The Impact bullet says how to migrate in moq-quic, moq-tokio, moq-uring, and
qmux if its public API changes.
