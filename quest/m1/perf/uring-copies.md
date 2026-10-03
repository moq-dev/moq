# [S] Remove moq-uring's avoidable copies

## Goal

moq-uring stops copying and allocating on paths where the QUIC core already
supports zero-copy, with a measured before/after.

## Plan

Found in the 2026-09-30 fork planning; none needs a QUIC API change:

- Egress: `quic/noq/connection.rs` copies each GSO train from `scratch` into
  the TX buffer. Hand the TX pool's `Vec` to `poll_transmit` directly; it
  encrypts in place and only reserves when capacity is short.
- Stream send: moq-net's `write_chunk(Bytes)` falls back to the trait's
  default `poll_write_buf`, which copies through `poll_write(&[u8])`. Route
  it to the core's `write_chunks` so large chunks are kept by reference.
- Stream receive: moq-uring uses the trait's default `poll_read_chunk`, which
  allocates an 8 KiB `BytesMut` and copies each read. Return the core's
  `Bytes` directly.
- Datagram receive: `endpoint.rs` allocates a `BytesMut` per GRO segment.
  Copy once per completion and split it into segments.

The clock-read fix stays in
[#3122](/quest/m1/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md).
Measure with the echo benches and `just bench`. Paths above are pre-fork names.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the copies are removed on `moq-quic`, not the frozen fork
