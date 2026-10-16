# [S] Unknown stream types survive TS export

## Goal

A verbatim track whose `streamType` is any byte the PMT allows exports with
that byte in the rebuilt PMT. Today import carries it as opaque, then export
fails building the PMT because `StreamType::from_u8` has no variant for it.

## Plan

`psi::encode_pmt` already writes the byte itself; only its input,
`mpeg2ts::ts::payload::Pmt`, holds the closed enum. Give the encoder its own
ES entry with a raw `u8` stream type and drop the conversion in
`Export::build_psi`. The importer then needs no refusal for unknown types.

Reproduce with a failing export test on an unknown type (for example 0x99
on a PES track and on a section track), then assert the PMT byte. Stream
types the importer refuses for ambiguity (0x86 without `CUEI`) stay with
[the fidelity plan](/quest/m1/ts-reliability/fidelity.md).
