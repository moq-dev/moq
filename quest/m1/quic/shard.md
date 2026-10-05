# [M] Shard the QUIC endpoint across cores

## Goal

A `moq-quic` server endpoint shards across cores by default where the
platform can steer (Linux): the library
forms the steered `SO_REUSEPORT` group and issues connection IDs that name
their shard, and each runtime (moq-tokio and moq-uring) only drives the
shards it is handed on threads of its own. The relay's `--workers` flag
becomes an optional count override, and the two copies of the group logic
are gone.

## Plan

Decided in the 2026-09-30 plan: the library shards and the runtime drives.
The library does not own threads, so embedders (libmoq, moq-ffi) keep their
own threading model.

- `moq-quic-udp` absorbs `moq-sock::shard` (the group, claims, and steering
  filter); `moq-sock` keeps binding and CPU helpers.
- `moq-quic` issues shard-prefixed connection IDs itself, replacing the
  custom CID generators in moq-tokio and moq-uring.
- moq-tokio's `worker/group.rs` shrinks to spawning one pinned
  `current_thread` runtime per shard; moq-uring's `udp::Bound` group code is
  replaced by the library's.
- A server endpoint defaults to one shard per core only where
  `SO_REUSEPORT` steering works, which today is Linux: `moq-sock` returns
  `Unsupported` for both load balancing and steering elsewhere. macOS and
  Windows keep a genuine single-socket endpoint, and an explicit count above
  one is refused there rather than silently ignored.
- Before flipping the default, add a benchmark sweeping shard count against
  concurrent session load (publishers and subscribers), so steering,
  dispatch, and per-shard overhead show up as slopes. A single
  default-versus-one-socket run cannot show a cost that grows with either
  axis.

Update `doc/bin/relay/` for the flag change.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the stack has to be in-tree before the library can own sharding

## Related

- [UDP demux](/quest/m2/one-port/udp-demux.md) - demuxes each shard's socket; keep the demux over whatever the library hands out
- [Steer only QUIC by connection ID](/quest/m2/one-port/shard-steering.md) - the filter this moves leaves non-QUIC flows to the kernel's 4-tuple hash
