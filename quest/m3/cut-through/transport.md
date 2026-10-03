# [M] Unordered reads and offset writes in web-transport-trait

## Goal

A released `web-transport-trait` lets a receive stream yield chunks with their
offsets in arrival order and lets a send stream write at an offset. The
`moq-quic` backends implement both; every other backend reports that it cannot.

## Plan

Decided 2026-09-30: additive methods with defaults, not a moq-net-private
extension trait, which would need per-session detection behind the generic
`Session`.

- `RecvStream` gains an unordered chunk read returning `(offset, Bytes)`, and
  `SendStream` gains an offset write, both in the poll style. Their defaults
  report unsupported in the return type (as
  [poll_acked](/quest/m2/quic-ack-hook.md) does, since a default cannot build
  `Self::Error`), and the caller falls back to ordered I/O.
- quinn's assembler, which `moq-quic` keeps, allows no ordered read after an
  unordered one, so the unordered mode is entered once per stream and never
  left.
- Implement both in the in-tree `web-transport-moq` over the unordered
  `read_chunk` and the [offset write](/quest/m3/cut-through/quic.md), and
  natively in moq-uring's QUIC stream (today `rs/moq-uring/src/quic/noq/stream.rs`,
  renamed by the [switch](/quest/m1/quic/fork/switch.md)), which reads ordered
  and drops the chunk offset. moq-tokio's poll adapter forwards them.
  `web-transport-wasm`, qmux, and iroh keep the defaults.
- Release `web-transport-trait` and bump its pin here.

## Required

- [Offset writes in moq-quic](/quest/m3/cut-through/quic.md) - the send-side primitive this exposes
