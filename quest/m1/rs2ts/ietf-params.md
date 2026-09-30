# [S] IETF parameters without primitive traits

## Goal

moq-net's IETF message parameters encode and decode through concrete
per-kind methods, with no `Param` impls on `u8`, `bool`, `u64`, or
`Option<T>`, so the IETF codec has the same concrete shape as the lite codec
the translator targets.

## Plan

The VarInt codec (#4463) removed Encode/Decode on
primitives, but `ietf/parameters.rs` still implements its own `Param` trait on
them, which rs2ts can only translate with dictionary passing. Replace the
impls with methods on the concrete `Decoder`/`Encoder` (or on the parameter
kinds), keeping the draft-14..16 varint cast and the draft-17+ forms byte for
byte. Benchmark IETF message encode and decode before and after with
`--bench codec`.

Public API: none (`ietf` parameters are crate-private). Lands on `dev` with
the line. Wire: none.

## Required

- #4463 (VarInt codec) merged into the questline, since this builds on its `Decoder`/`Encoder`
