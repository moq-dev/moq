# [M] Tiny QUIC frames cannot pin receive batches

## Goal

A peer cannot make a moq-quic receiver hold more memory than its receive
budgets by sending tiny DATAGRAM or STREAM frames. Each queued datagram and
each leased chunk retains only its own bytes, never the receive batch it
arrived in, on both the tokio and io_uring drivers.

## Plan

Found in the 2026-10-23 moq-quic audit; reproduce before fixing.

The tokio driver copies each `recvmmsg` batch into one `BytesMut` and splits
datagrams off it (`moq-tokio/src/quic/driver/endpoint.rs`, the receive loop),
and a DATAGRAM frame's payload is a zero-copy slice of that allocation
(`moq-quic/src/frame.rs`, `Frame::Datagram`). `DatagramState::received`
(`connection/datagrams.rs`) charges only `len + size_of::<Datagram>()`, so a
1-byte datagram is charged about 33 bytes while it keeps its whole batch
alive. The default `datagram_receive_buffer_size` (about 1.25 MB) is
advertised even when nothing reads datagrams, as on a moq-lite session that
never enabled them, so the queue never drains: roughly 38k retained
datagrams, each pinning up to one batch (up to 64 KB with GRO).

Leases have the same shape: `Leased::compact` runs only on the `Blocked`
path, and `take()` hands out zero-copy slices. moq-net leases with
`want = 1`, so a peer trickling an incomplete frame in 1-byte STREAM frames
pins one batch per byte. The README's "a byte of credit never pins a receive
batch" holds only for readers that wait.

Fix at the source, the way `mux` already does
(`connection/mux/tests.rs`, `a_small_datagram_does_not_pin_its_record`):

- Copy DATAGRAM payloads into their own allocation on receive.
- Copy leased chunks out of the batch in `take()`, or compact on every
  `fill`, whichever the `receive` bench shows is cheaper. `take()` is
  documented as zero-copy: copying there changes that doc in the same PR,
  and fill-time compaction keeps it. The pinning tests hold either way.
- Check moq-uring's receive path the same way; its buffer-ring slots may
  pin differently.

Tests: the mux-style pinning assertion on the QUIC datagram path and on
lease `take()`, for both drivers. Measure the copy on `benches/receive.rs`
and report it in the PR.

## Related

- [QUIC I/O boundary](/quest/m2/quic-io-boundary.md) - redesigns receive buffer ownership; must keep this bound
- [Lease bookkeeping](/quest/m1/quic-lease-bookkeeping.md) - the other lease fixes from the same audit
