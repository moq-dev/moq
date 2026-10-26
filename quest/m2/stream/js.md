# [S] JS byte streams

## Goal

`@moq/net` dials and accepts the same byte streams as Rust, exposed as a
WHATWG `ReadableStream`/`WritableStream` pair, and interoperates with the Rust
side in both directions.

## Plan

Mirror the [Rust surface](/quest/m2/stream/rust.md) and its path layout. Add
the Rust/JS pair to `just test interop`.

## Required

- [Rust byte streams](/quest/m2/stream/rust.md) - the reference surface and path layout
