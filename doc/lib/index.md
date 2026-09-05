---
title: Libraries
description: MoQ libraries for Rust, TypeScript, C, Python, Kotlin, Swift, Go, and Dart
---

# Libraries

Two primary implementations and six bindings, all speaking the same wire
protocol. A publisher in Python is consumable by a subscriber in Swift.

| Language | Package | Best for |
| --- | --- | --- |
| [Rust](/lib/rs/) | `moq-net`, `hang`, and friends on crates.io | Servers, CLIs, native apps, anything that needs the full stack including hardware codecs. |
| [TypeScript](/lib/js/) | `@moq/*` on npm | Browsers (WebTransport + WebCodecs) and Node/Bun/Deno. |
| [Swift](/lib/swift/) | `Moq` via SwiftPM | iOS, iPadOS, macOS. |
| [Kotlin](/lib/kt/) | `dev.moq:moq` on Maven Central | Android and the JVM. |
| [Python](/lib/py/) | `moq-rs` on PyPI | Scripts, ML pipelines, voice agents. |
| [Go](/lib/go/) | `github.com/moq-dev/moq-go` | Go services and tooling. |
| [Dart](/lib/dart/) | `moq` on pub.dev | Flutter apps. |
| [C](/lib/c/) | `libmoq` | C/C++ and any language with a C FFI. |

## How they relate

Rust is the reference implementation; every server-side tool is built on it.
TypeScript is a from-scratch browser implementation. The other six wrap the
Rust core: Python, Kotlin, Swift, Go, and Dart are generated from one
[UniFFI](https://mozilla.github.io/uniffi-rs/) crate (`moq-ffi`) and then
wrapped in an idiomatic layer, while C gets a hand-written stable ABI
(`libmoq`). A feature added to the core lands in all of them together.

## What every binding can do

The wrapped bindings share one feature set, so the language pages only show
how it looks in that language:

- **Connect** to a relay with TLS options (system roots, custom CA, fingerprint pinning, mTLS) and a JWT in the URL, or **serve** sessions yourself and accept or reject each request by path.
- **Discover** broadcasts by prefix, wait for a specific one, or request an unannounced one.
- **Publish and subscribe to media** with the hang catalog filled in from the bitstream, plus raw pixels or PCM in and out with the codec running inside the binding (VideoToolbox, Media Foundation, NVENC, openh264, Opus).
- **Raw tracks** of arbitrary bytes with timestamps, sparse or replayed groups, per-subscriber priority and latency, and best-effort datagrams.
- **JSON tracks** in snapshot mode (latest value, merge-patch deltas, optional compression) or stream mode (append log).
- **Fetch** a single group by sequence from the cache, decoded through the container or raw.
- **Serve on demand**: accept track and broadcast requests as they arrive instead of publishing up front.
- **Catalog extensions**: write your own section next to `video` and `audio`, and read others' back.
- **Routes**: see which relays a broadcast came through, and advertise a cost as a standby publisher.
- **Errors** distinguish auth rejection (don't retry) from shutdown (expected) from transport failure.

Dart is the exception on codecs: its published binaries carry no encoder or
decoder, so it moves already-encoded frames.
