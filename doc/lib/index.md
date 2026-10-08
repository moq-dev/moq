---
title: Libraries
description: MoQ libraries for Rust, TypeScript, C, Python, Kotlin, Swift, Go, and Dart
---

# Libraries

Two primary implementations and six bindings, all speaking the same wire
protocol. A publisher in Python is consumable by a subscriber in Swift.

| Language | Package | Best for |
| --- | --- | --- |
| <img class="language-icon" src="/icons/languages/rust.svg" alt="" /> [Rust](/lib/rs/) | `moq-net`, `hang`, and friends on crates.io | Servers, CLIs, native apps, anything that needs the full stack including hardware codecs. |
| <img class="language-icon" src="/icons/languages/typescript.svg" alt="" /> [TypeScript](/lib/js/) | `@moq/*` on npm | Browsers (WebTransport + WebCodecs) and Node/Bun/Deno. |
| <img class="language-icon" src="/icons/languages/swift.svg" alt="" /> [Swift](/lib/swift/) | `Moq` via SwiftPM | iOS, iPadOS, macOS. |
| <img class="language-icon" src="/icons/languages/kotlin.svg" alt="" /> [Kotlin](/lib/kt/) | `dev.moq:moq` on Maven Central | Android and the JVM. |
| <img class="language-icon" src="/icons/languages/python.svg" alt="" /> [Python](/lib/py/) | `moq-rs` on PyPI | Scripts, ML pipelines, voice agents. |
| <img class="language-icon" src="/icons/languages/go.svg" alt="" /> [Go](/lib/go/) | `moq.dev/moq` | Go services and tooling. |
| <img class="language-icon" src="/icons/languages/dart.svg" alt="" /> [Dart](/lib/dart/) | `moq` on pub.dev | Flutter apps. |
| <img class="language-icon" src="/icons/languages/c.svg" alt="" /> [C](/lib/c/) | `moq-c` | C/C++ and any language with a C FFI. |

## How they relate

Rust is the reference implementation; every server-side tool is built on it.
TypeScript is a from-scratch browser implementation. The other six wrap the
Rust core: Python, Kotlin, Swift, Go, and Dart are generated from one
[UniFFI](https://mozilla.github.io/uniffi-rs/) crate (`moq-ffi`) and then
wrapped in an idiomatic layer, while C gets a hand-written stable ABI
(`moq-c`). A feature added to the core lands in all of them together.

## What every binding can do

The wrapped bindings share one feature set, so the language pages only show
how it looks in that language:

- **Connect** to a relay with TLS options and a JWT in the URL, or **serve** sessions yourself and accept or reject each request by path.
- **Reconnect** automatically with backoff when the transport drops.
- **Discover** broadcasts by prefix, wait for a specific one, or request one by path. Advertise an exact path (a created broadcast is invisible until announced), or claim a prefix and serve requests beneath it on demand.
- **Publish and subscribe to media** with the hang catalog filled in from the bitstream, or hand over raw pixels and PCM and let the binding encode and decode (VideoToolbox, Media Foundation, NVENC, openh264, Opus). The built-in video encoder can follow the connection's send estimate.
- **Raw tracks** of arbitrary bytes, with per-subscriber priority and max delay, and best-effort datagrams.
- **JSON tracks** as a latest value with merge-patch deltas, or as an append log.
- **Fetch** a single cached group by sequence.
- **Catalog extensions**: write your own section next to `video` and `audio`, and read others' back.
- **Routes**: see which relays a broadcast came through, and advertise a cost as a standby publisher.
- **Connection stats** and **errors** that tell an auth rejection (don't retry) from a shutdown or a transport failure.

Dart is the exception on codecs: its published binaries carry no encoder or
decoder, so it moves already-encoded frames.
