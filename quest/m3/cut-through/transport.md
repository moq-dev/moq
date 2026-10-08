# [M] Unordered reads and offset writes in moq-net's transport traits

## Goal

moq-net's own poll transport traits (`transport::poll` in
`rs/moq-net/src/transport.rs`, since #4709) let a receive stream yield chunks
with their offsets in arrival order and let a send stream write at an offset.
The `moq-quic` backends implement both; every other backend reports that it
cannot.

## Plan

Decided 2026-09-30: additive methods with defaults, not a separate extension
trait, which would need per-session detection behind the generic `Session`.
Re-planned in the 2026-10-08 audit onto `transport::poll`, as
[poll_acked](/quest/m2/quic-ack-hook.md) was: moq-net no longer depends on
`web-transport-trait`, so no upstream release is needed.

- `RecvStream` gains an unordered chunk read returning `(offset, Bytes)`, and
  `SendStream` gains an offset write, both in the poll style. Their defaults
  report unsupported in the return type (as
  [poll_acked](/quest/m2/quic-ack-hook.md) does, since a default cannot build
  `Self::Error`), and the caller falls back to ordered I/O.
- quinn's assembler, which `moq-quic` keeps, allows no ordered read after an
  unordered one, so the unordered mode is entered once per stream and never
  left.
- Implement both in moq-tokio's adapter over the in-tree `web-transport-moq`
  (its unordered `read_chunk` and the [offset write](/quest/m3/cut-through/quic.md)),
  and natively in moq-uring's QUIC stream (today `rs/moq-uring/src/quic/noq/stream.rs`,
  renamed by the [switch](/quest/m1/quic/fork/switch.md)), which reads ordered
  and drops the chunk offset. The WASM, qmux, and iroh adapters keep the
  defaults.

## Required

- [Offset writes in moq-quic](/quest/m3/cut-through/quic.md) - the send-side primitive this exposes
