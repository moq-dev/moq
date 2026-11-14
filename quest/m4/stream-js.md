# [S] JS byte streams

## Goal

`@moq/net` dials and accepts the same byte streams as Rust, exposed as a
WHATWG `ReadableStream`/`WritableStream` pair, and interoperates with the Rust
side in both directions.

## Plan

Mirror the Rust surface, `moq_tokio::socket::TcpStream`, on `@moq/net`'s socket. `dialTcp` and `accept` already take the
`priority` option Rust's `dial_tcp` and `accept` do. Add
the Rust/JS pair to `just test interop`.

`@moq/net`'s socket `TcpStream` already exists and the socket interop test
(`test/interop/socket.sh`) runs, so this adds only the WHATWG adapter. The
JS tail is unpaced today (`js/net/src/socket.ts`: a reader falling behind it
fails with eviction), so a slow JS reader is not lossless until JS paces by
its fastest reader.

Moved to m4 2026-11-12 (maintainer, /quest-audit): no downstream application
waits on JS byte streams, and they are lossless only after JS pacing lands.

## Required

- [JS fastest-reader pacing](/quest/m4/js-fastest-reader.md) - a JS reader that falls behind would otherwise end evicted, not lossless
