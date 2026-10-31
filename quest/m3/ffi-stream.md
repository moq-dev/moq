# [M] Byte streams in moq-ffi

## Goal

The Swift and Kotlin bindings dial and accept the same byte streams as Rust
and JS: a duplex stream to a peer by path with ordered reads and writes,
half-close, and reset with a code, interoperating with `@moq/net` in both
directions. Non-goal: a TCP forwarder on the device; the mobile consumer reads
and writes the stream in-process.

## Plan

Decided 2026-10-28 with the maintainer (moq.pro T3 planning interview): the
named consumer is moq.pro's
[T3 Code over MoQ](https://github.com/moq-dev/moq.pro/blob/main/quest/m3/t3/README.md)
line, whose React Native app reaches its host through a native module over
these bindings instead of the WebSocket fallback, so a lossy mobile link keeps
QUIC's per-stream recovery.

Wrap the [Rust surface](/quest/m2/stream/rust.md) in `moq-ffi` with the same
path layout and priority knob, and surface it through the generated Swift and
Kotlin packages. Use an async read and write shape that matches how the
bindings already expose frames; report the choice. Add a Kotlin or Swift
binding test against a JS peer to the interop suite.

## Required

- [Rust byte streams](/quest/m2/stream/rust.md) - the surface these bindings wrap
- [JS byte streams](/quest/m2/stream/js.md) - the peer the interop test runs against
