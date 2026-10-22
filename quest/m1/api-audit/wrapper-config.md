# [M] Wrapper config names

## Goal

Python, Go, and Dart client options use the FFI's names and nesting
(`tls.insecure`, `once`, nested `tls`, `quic`, `websocket`, `backoff`), as
Swift, Kotlin, and C++ already do, and Go takes an options struct as
`rs/moq-ffi/AGENTS.md` requires.

## Plan

Decided 2026-10-22 (API audit). #4519 renamed the FFI to moq-tokio's names;
Python (`py/moq-rs/.../client.py` `tls_verify`, `reconnect`), Go
(`WithTLSVerify`, `WithReconnect`, now 25 functional options), and Dart
(`tlsVerify`) kept the old ones.

- Rename to the FFI names and nesting. Keep each language's idiomatic
  duration type at the wrapper boundary;
  [binding durations](/quest/m2/ffi-durations.md) moves the FFI itself off
  `_us` integers later.
- Go: replace functional options with a config struct.

Public API: Python, Go, Dart wrappers breaking. Wire: none.
