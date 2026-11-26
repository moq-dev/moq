# [L] Encode Opus without unsafe code

## Goal

`rs/moq-opus`'s encode path (float encode with bitrate, DTX, signal, and
application settings) runs on the picked path and passes the harness, and
the whole crate compiles under a crate-level `#![forbid(unsafe_code)]`, its
remaining c2rust code deleted. moq-audio encodes through that safe API.

## Required

- [Decode Opus without unsafe code](/quest/m3/opus-safe/decoder.md) - settles the path and lands the decoder half
