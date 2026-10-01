# [M] VarInt codec

## Goal

Every varint moq-net puts on the wire is a `VarInt`, and `VarInt` is the only
integer type with Encode/Decode. Messages encode and decode through a
concrete, slice-based codec instead of generic traits implemented on `u64`,
`usize`, `bool`, `String`, `Option<T>`, and `Vec<T>`.

## Plan

Two reasons, one refactor. Not every `u64` is a valid varint, so the type
should say which fields are. And the rs2ts translator cannot map generic
traits on primitives without dictionary passing, its most expensive feature;
the same generics are most of why moq-net's lite codec built to 47 KB gzip in
WASM against 6 KB for a hand-carved one.

Guidance:

- Widen `VarInt` to the full 64 bits that leading-ones carries on moq-lite-07
  and moq-transport draft-17+, and never bound it to 2^53. QUIC varints still
  refuse to encode past 2^62-1. On lite-07 this lets Rust delete the
  `BoundsExceeded` arm in the lite dispatch and flips `lite_varint_interop` from
  refusal to a round trip. Also revisit `MAX_COST`: costs saturate at 2^62-1 on
  every version, but the draft caps them at the largest value a varint carries.
- Message types keep a local trait; the primitives become inherent methods
  on concrete reader and writer types (`varint`, `string`, `bytes`, ...).
  Make the version a concrete type rather than a generic `V` where possible.
- Avoid bit operations on `u64` in the codec: write the 8-byte form as two
  `u32` halves, so the generated TypeScript never needs 64-bit bitwise math.
- `Parameters` becomes Vec-backed, and decode paths stop branching on
  `tracing::enabled!` (log after decoding instead).
- Benchmark the codec before and after (Criterion); it is on every message.

Public API: breaks moq-net's `coding` module (Encode/Decode on primitives
go away), so this retargets to `dev`. Wire: none.
