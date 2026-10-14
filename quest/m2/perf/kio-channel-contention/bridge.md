# [M] In-memory transport

## Goal

moq-net ships an in-memory `poll::Session` pair: two moq-lite peers in one
process talk without QUIC, sockets, or crypto, and a stream frame payload
crosses as the same `Bytes` (no copy). Streams are flow-controlled like QUIC, so a slow
reader throttles its own stream and nothing else. The test `MockSession`
becomes fault knobs layered over it.

## Plan

Decided (maintainer, 2026-10-14):

- A public module in moq-net beside the `poll` traits
  (`rs/moq-net/src/transport.rs`), not a new crate. Propose the module and
  type names in the PR. It implements moq-net's own `poll::Session`, not the
  external `web-transport-trait`, which moq-net does not depend on.
- Zero-copy: override `poll_write_buf` (`Buf::copy_to_bytes` is free for a
  `Bytes` source) and `poll_read_chunk`, and keep write boundaries so one
  written chunk is read back as one chunk. A whole-frame chunk then lands in
  `FrameStorage::Shared` through `try_set_bytes`
  (`rs/moq-net/src/model/frame.rs`); only the small headers are copied.
  Datagrams are copied: `poll_send_datagram` borrows a `&[u8]` it cannot
  retain, and they are small, so changing that API is out of scope.
- Flow control per stream and per session, counted in bytes held: a writer
  out of credit pends until the reader consumes. Reset and stop-sending work
  as in QUIC, so a lagging peer's stale groups are reset rather than
  buffered. Backpressure throttles the stream it applies to, never the
  publisher behind it.
- Rebuild `rs/moq-net/tests/support/mock.rs` on it: its latency, held unis,
  and lost datagrams become knobs over the bridge, deleting the duplicate,
  copying stream plumbing, so tests exercise the zero-copy path. Check
  `rs/moq-e2ee/tests/support/mock.rs`, which carries a similar pair.
- `Send`, so the two ends can live on different worker threads, and it
  meets the bounds the handshakes need: `Client::connect` requires
  `poll::Boxable`, and `Server::accept` also requires `MaybeSync` streams.
- A bench of frame throughput over the bridge, swept over streams and frame
  size, with an allocation count that proves payloads aren't copied.

## Related

- [Replica spike](/quest/m2/perf/kio-channel-contention/spike-replica.md) - links worker origins over this
- [Cluster spike](/quest/m2/perf/kio-channel-contention/spike-cluster.md) - links in-process relays over this
