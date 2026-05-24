---
title: Libraries
description: MoQ libraries for Rust, TypeScript, C, Python, Kotlin, Swift, and Go
---

# Libraries

MoQ ships libraries in a handful of languages. **Rust** (native) and **TypeScript** (web) are the primary implementations; everything else wraps the Rust core under the hood.

## Primary

### [Rust](/lib/rs/) <Badge type="tip" text="native" />

The reference implementation. Used by every server-side tool and by the FFI core that the other language bindings link against.

- [`moq-net`](/lib/rs/crate/moq-net) - Real-time pub/sub
- [`hang`](/lib/rs/crate/hang) - Media catalog and container
- [`moq-mux`](/lib/rs/crate/moq-mux) - fMP4/CMAF/HLS import
- [`moq-native`](/lib/rs/crate/moq-native) - QUIC endpoint helpers
- [...and more](/lib/rs/)

### [TypeScript](/lib/js/) <Badge type="tip" text="web" />

The browser implementation. Uses [WebTransport](/concept/layer/web-transport), WebCodecs, and WebAudio to run MoQ natively in the browser without polyfills (in supported browsers).

- [`@moq/net`](/lib/js/@moq/net) - Real-time pub/sub
- [`@moq/hang`](/lib/js/@moq/hang/) - Media library
- [`@moq/watch`](/lib/js/@moq/watch) - Subscribe + render
- [`@moq/publish`](/lib/js/@moq/publish) - Capture + publish
- [...and more](/lib/js/)

## FFI bindings

These all link against the same [Rust core](https://crates.io/crates/moq-ffi) (via [`libmoq`](/lib/rs/crate/libmoq) + UniFFI) and present an idiomatic API in their host language.

### [C](/lib/c/)

Raw C bindings via `libmoq`. The lowest-level entry point and the foundation for every other binding listed below.

### [Python](/lib/py/)

Async/await with `asyncio`. Published as [`moq-net`](https://pypi.org/project/moq-net/) on PyPI.

### [Kotlin](/lib/kt/)

Coroutines and `Flow` for Android and the JVM. Published as `dev.moq:moq` on Maven Central.

### [Swift](/lib/swift/)

Async sequences and structured concurrency for iOS, iPadOS, and macOS. Distributed via Swift Package Manager.

### [Go](/lib/go/)

cgo bindings with prebuilt static libraries per platform. Resolved via `go get github.com/moq-dev/moq-go`.

## Picking a language

- **Server, CLI, or anything native** &rarr; [Rust](/lib/rs/)
- **Web browser or Node/Bun/Deno** &rarr; [TypeScript](/lib/js/)
- **iOS / macOS app** &rarr; [Swift](/lib/swift/)
- **Android app or JVM service** &rarr; [Kotlin](/lib/kt/)
- **Scripts, ML pipelines, prototypes** &rarr; [Python](/lib/py/)
- **Go service or tooling** &rarr; [Go](/lib/go/)
- **Anything else with a C ABI** &rarr; [C](/lib/c/)

All FFI bindings expose the same protocol surface as the Rust core, so a publisher in Python can be consumed by a Swift subscriber, etc.
