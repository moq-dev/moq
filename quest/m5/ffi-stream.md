# [M] Byte streams in moq-ffi

## Goal

The Swift and Kotlin bindings dial and accept the same byte streams as Rust
and JS: a duplex stream to a peer by path with ordered reads and writes,
half-close, and reset with a code, interoperating with `@moq/net` in both
directions. Non-goal: a TCP forwarder on the device; the mobile consumer reads
and writes the stream in-process.

## Plan

Decided 2026-10-28 with the maintainer (a downstream planning interview):
planned for a downstream application's React Native app, to reach its host
through a native module over these bindings instead of the WebSocket
fallback, so a lossy mobile link keeps QUIC's per-stream recovery.

No named consumer remains (checked 2026-11-12, /quest-audit): that
application's mobile module now wraps a WebTransport binding rather than
moq-ffi, and carries its traffic in groups rather than byte streams. It stays
parked here until a binding user asks for byte streams.

Wrap the Rust surface, `moq_tokio::socket::TcpStream`, in `moq-ffi` with the same
path layout and priority knob (`dial_tcp` and `accept` take a `priority`, which
`moq-ffi` and `moq-c` pass as 0 until then), and surface it through the generated Swift and
Kotlin packages. Use an async read and write shape that matches how the
bindings already expose frames; report the choice. Add a Kotlin or Swift
binding test against a JS peer to the interop suite.

## Required

- [JS byte streams](/quest/m4/stream-js.md) - the peer the interop test runs against
