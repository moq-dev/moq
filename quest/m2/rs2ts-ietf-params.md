# [S] IETF parameters without primitive traits

## Goal

moq-net's IETF message parameters encode and decode through concrete
per-kind methods, with no `Param` impls on `u8`, `bool`, `u64`, or
`Option<T>`, so the IETF codec has the same concrete shape as the lite codec
the translator targets.

## Plan

The VarInt codec removed Encode/Decode on primitives, but
`ietf/parameters.rs` still implements its own `Param` trait on them, which rs2ts can only translate with dictionary passing. Replace the
impls with methods on the concrete `Decoder`/`Encoder` (or on the parameter
kinds), keeping the draft-14..16 varint cast and the draft-17+ forms byte for
byte. Keep the per-draft parameter lists and the unknown and misplaced
parameter rules that [#5028](https://github.com/moq-dev/moq/pull/5028)
added; this is a
refactor, not a behavior change. Benchmark IETF message encode and decode
before and after with `--bench codec`.

Public API: none (`ietf` parameters are crate-private). Wire: none.

Decided in the 2026-09-30 audit: deferred to m2 with generated IETF, its only
consumer, until generated lite passes its go/no-go.

