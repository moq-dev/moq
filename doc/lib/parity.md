---
title: Binding parity
description: Capability parity across moq-ffi, language wrappers, and libmoq
---

# Binding parity

`moq-ffi` is the canonical shared API for Python, Kotlin, Swift, and Go. Their raw generated bindings expose every `moq-ffi` object, record, enum, and method. The ergonomic packages rename those types and adapt async iteration, cancellation, errors, and options to each language without removing capabilities.

`libmoq` is a separate callback-based C ABI. It shares `moq-native`, `moq-net`, `moq-mux`, `moq-audio`, and `moq-json` with `moq-ffi`, but it is not the implementation underneath UniFFI.

## Application capability matrix

| Capability | Python | Kotlin | Swift | Go | C (`libmoq`) |
|---|---:|---:|---:|---:|---:|
| Client sessions and connection stats | Yes | Yes | Yes | Yes | Yes |
| Server listen and accept | Yes | Yes | Yes | Yes | No |
| Client TLS roots, system roots, fingerprints, mTLS, and bind | Yes | Yes | Yes | Yes | No |
| Announce, discover, and request broadcasts | Yes | Yes | Yes | Yes | Yes |
| Dynamically requested broadcasts | Yes | Yes | Yes | Yes | No |
| Raw tracks, explicit groups, timestamps, and aborts | Yes | Yes | Yes | Yes | Yes |
| Subscription controls and track metadata | Yes | Yes | Yes | Yes | Yes |
| One-shot group fetch and dynamic cache misses | Yes | Yes | Yes | Yes | No |
| Best-effort track datagrams | Yes | Yes | Yes | Yes | Yes |
| Whole-frame media import and catalog hints | Yes | Yes | Yes | Yes | Partial |
| Byte-stream media import | Yes | Yes | Yes | Yes | No |
| Catalog media records and application sections | Yes | Yes | Yes | Yes | Yes |
| Raw PCM audio encode and decode | Yes | Yes | Yes | Yes | Yes |
| JSON snapshot and stream tracks | Yes | Yes | Yes | Yes | Yes |
| Native decoded video frames | No | No | No | No | Yes |

"Partial" means `libmoq` imports whole media frames and lets callers edit catalog records, but does not yet accept the `moq-ffi` video hint object or a dynamically requested media track.

## Rust-only surface

The binding layer intentionally stops above implementation plumbing. Low-level QUIC and WebTransport traits, protocol message codecs, relay internals, chunked frame readers and writers, generic catalog extensions, container exporters, and individual codec parsers remain Rust APIs. They are building blocks for implementing a binding, not a portable application API.

The main remaining parity work is in `libmoq`: server support, configurable client TLS, origin and track dynamic requests, one-shot group fetch, and streaming media import. The table is updated whenever one of those capabilities crosses the C boundary.
