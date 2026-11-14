# [S] JS byte streams

## Goal

`@moq/net` dials and accepts the same byte streams as Rust, exposed as a
WHATWG `ReadableStream`/`WritableStream` pair, and interoperates with the Rust
side in both directions.

## Plan

Mirror the [Rust surface](/quest/m1/stream/rust.md) on `@moq/net`'s socket. Add
the Rust/JS pair to `just test interop`.

`@moq/net`'s socket `TcpStream` already exists and the socket interop test
(`test/interop/socket.sh`) runs, so this adds only the WHATWG adapter. The
JS tail is unpaced today (`js/net/src/socket.ts`: a reader falling behind it
fails with eviction), so a slow JS reader is not lossless until JS paces by
its fastest reader.

Moved to m4 2026-11-12 (maintainer, /quest-audit): no downstream application
waits on JS byte streams, and they are lossless only after JS pacing lands.

## Required

- [Rust byte streams](/quest/m1/stream/rust.md) - the reference surface
- [JS fastest-reader pacing](/quest/m4/js-fastest-reader.md) - a JS reader that falls behind would otherwise end evicted, not lossless
