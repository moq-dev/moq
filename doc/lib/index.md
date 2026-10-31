---
title: Libraries
description: MoQ libraries for Rust, TypeScript, C++, C, Python, Kotlin, Swift, Go, and Dart
---

# Libraries

Two primary implementations and seven bindings, all speaking the same wire
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
| [C++](/lib/cpp/) | `moq-cpp` via CMake or pkg-config | C++ apps, engines, and plugins. |
| <img class="language-icon" src="/icons/languages/c.svg" alt="" /> [C](/lib/c/) | `moq-c` | The plain-C ABI, for C and any language with a C FFI; C++ should use the package above. |

## Choose the layer

| You need | Start with |
| --- | --- |
| A browser player or camera publisher | [@moq/watch](/lib/js/watch) and [@moq/publish](/lib/js/publish), with web components or a headless API |
| A meeting roster and chat | [@moq/room](/lib/js/room) or [moq-room](/lib/rs/moq-room) |
| Native media capture, encode, decode, or playback | [Rust audio](/lib/rs/moq-audio) and [video](/lib/rs/moq-video), or the media and codec layers in your language binding |
| To import an existing media stream | [moq-mux](/lib/rs/moq-mux), or a [gateway](/bin/#gateways) if you want a ready-to-run tool |
| State snapshots, chat logs, or other JSON data | [@moq/json](/lib/js/json), [moq-json](/lib/rs/moq-json), or your binding's [JSON layer](/lib/layers#json) |
| Custom payloads and subscription behavior | [@moq/net](/lib/js/net) or [moq-net](/lib/rs/moq-net), also exposed by the bindings |

[Examples](/setup/examples) links runnable consumers. [Layers](/lib/layers)
maps the native binding namespaces; the language pages link API references.

## How they relate

Rust is the reference implementation; every server-side tool is built on it.
TypeScript is a from-scratch browser implementation. The other seven wrap the
Rust core: Python, Kotlin, Swift, Go, Dart, and C++ are generated from one
[UniFFI](https://mozilla.github.io/uniffi-rs/) crate (`moq-ffi`) and then
wrapped in an idiomatic layer, while C gets a hand-written stable ABI
(`moq-c`). A feature added to the core lands in all of them together.
[Layers](/lib/layers) shows where each layer they wrap lives in every binding.

## Performance

The [benchmark trends](https://git.moq.dev/bench/) compare binding overhead
and show how it changes over time. Binding cases run in process, without
network or codec work, measuring throughput, CPU per byte, and synchronous
call latency. Each wrapper is compared with `rust-ffi`, which calls
`moq-ffi` directly from Rust. The `rust-net` cases call `moq-net` directly,
showing what `moq-ffi` adds through its handles and runtime.
Ratios above 1 mean more cost than `rust-ffi`, including throughput ratios,
which divide Rust's throughput by the wrapper's.

Each row shows its latest measurement, baseline, and recent history. Compare
binding ratios within the same run: absolute results depend on the machine.
These measurements help choose a language for the work crossing the binding;
application performance also includes its network, codecs, and own processing.

## What every binding can do

The wrapped bindings share one feature set, so the language pages only show
how it looks in that language:

- **Connect** to a relay with TLS options and a JWT in the URL, or **serve** sessions yourself and accept or reject each request by path.
- **Reconnect** automatically with backoff when the transport drops.
- **Discover** broadcasts by prefix, wait for a specific one, or request one by path. Advertise an exact path (a created broadcast is invisible until announced), or claim a prefix and serve requests beneath it on demand.
- **Publish and subscribe to media** with the hang catalog filled in from the bitstream, or hand over raw pixels and PCM and let the binding encode and decode (VideoToolbox, Media Foundation, NVENC, openh264, Opus). The built-in video encoder can follow the connection's send estimate. The audio decoder reports the playout delay to buffer ahead of the playhead, fixed or estimated from arrivals, so read each frame as soon as the last is handled rather than pacing reads to the speaker.
- **Raw tracks** of arbitrary bytes, with per-subscriber priority and max delay, and best-effort datagrams.
- **JSON tracks** as a latest value with merge-patch deltas, an append log, or a sliding window of records. Each value reads back with its frame timestamp, and a snapshot reader either replays every state or skips to the newest.
- **Fetch** a single cached group by sequence.
- **Catalog extensions**: write your own section next to `video` and `audio`, and read others' back.
- **Routes**: see which relays a broadcast came through, and advertise a cost as a standby publisher.
- **Connection stats** and **errors** that tell an auth rejection (don't retry) from a shutdown or a transport failure.
- **Authorization in band**: read the grant the relay sent back (the patterns this side may publish and subscribe to, and when it lapses), and present a fresh token before the current one expires, without reconnecting. It needs `moq-lite-07-wip`, which is opt-in, or moq-transport draft-18+ with the [MoQ Auth extension](/draft/moq-auth).

C's audio decoder neither takes nor reports a playout delay, and C has no in-band authorization.

## Upgrading

Each package keeps a `MIGRATE.md` saying how to migrate past every breaking
release, written when that release is cut;
[Upgrade](/setup/upgrade#migration-guides) links them all.
