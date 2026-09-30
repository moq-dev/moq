# [M] Shard the QUIC endpoint across cores

## Goal

A `moq-quic` server endpoint shards across cores by default: the library
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
- A server endpoint defaults to one shard per core. Measure the default
  against a single socket with `just bench` before flipping it, since the
  relay's default changes.

Update `doc/bin/relay/` for the flag change.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the stack has to be in-tree before the library can own sharding

## Related

- [UDP demux](/quest/m2/one-port/udp-demux.md) - demuxes each shard's socket; keep the demux over whatever the library hands out
