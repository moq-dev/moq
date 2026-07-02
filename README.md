<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)
[![Discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)
[![Crates.io](https://img.shields.io/crates/v/moq-net)](https://crates.io/crates/moq-net)
[![npm](https://img.shields.io/npm/v/@moq/net)](https://www.npmjs.com/package/@moq/net)

# Media over QUIC

[Media over QUIC](https://moq.dev) (MoQ) is a next-generation live media protocol that provides **real-time latency** at **massive scale**.
Built using modern web technologies, MoQ delivers WebRTC-like latency without the constraints of WebRTC.
The core networking is delegated to a QUIC library but the rest is in application-space, giving you full control over your media pipeline.

**Key Features:**

- 🚀 **Real-time latency** using QUIC for prioritization and partial reliability.
- 📈 **Massive scale** designed for fan-out and supports cross-region clustering.
- 🌐 **Modern Web** using [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API), [WebCodecs](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API), and [WebAudio](https://developer.mozilla.org/en-US/docs/Web/API/Web_Audio_API).
- 🎯 **Multi-language** with both Rust (native) and TypeScript (web) libraries.
- 🔧 **Generic** for any live data, not just media. Includes text chat as both an example and a core feature.

> **Note:** This project implements [moq-lite](https://doc.moq.dev/concept/layer/moq-lite), a forwards-compatible subset of the IETF [moq-transport](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/) draft. moq-lite works with any moq-transport CDN (ex. [Cloudflare](https://moq.dev/blog/first-cdn/)). The focus is narrower, prioritizing simplicity and deployability.

## Getting Started

Full documentation lives at **[doc.moq.dev](https://doc.moq.dev)**.

- **[Run the demo](https://doc.moq.dev/setup/demo/web)** - try MoQ locally with a relay, demo media, and the web UI.
- **[Linux packages](https://doc.moq.dev/setup/linux)** - install the relay and GStreamer plugin from `apt.moq.dev` / `rpm.moq.dev`.
- **[Production setup](https://doc.moq.dev/setup/prod)** - deploy a relay with a real domain and TLS.

The quickest way to see it in action (requires [Nix](https://nixos.org/download.html) with [flakes](https://nixos.wiki/wiki/Flakes)):

```sh
# Runs a relay, demo media, and the web server
nix develop -c just
```

Then visit <https://localhost:8080>. Don't have Nix? See the [demo guide](https://doc.moq.dev/setup/demo/web) for manual setup.

## Architecture

MoQ is designed as a layered protocol stack.

**Rule 1**: The CDN MUST NOT know anything about your application, media codecs, or even the available tracks.
Everything could be fully E2EE and the CDN wouldn't care. **No business logic allowed**.

Instead, [`moq-relay`](rs/moq-relay) operates on rules encoded in the [`moq-net`](https://docs.rs/moq-net) header.
These rules are based on video encoding but are generic enough to be used for any live data.
The goal is to keep the server as dumb as possible while supporting a wide range of use-cases.

The media logic is split into another protocol called [`hang`](https://docs.rs/hang).
It's pretty simple and only intended to be used by clients or media servers.
If you want to do something more custom, then you can always extend it or replace it entirely.

Think of `hang` as like HLS/DASH, while `moq-lite` is like HTTP.

```
┌─────────────────┐
│   Application   │   🏢 Your business logic
│                 │    - authentication, non-media tracks, etc.
├─────────────────┤
│      hang       │   🎬 Media-specific encoding/streaming
│                 │     - codecs, containers, catalog
├─────────────────├
│    moq-lite     │  🚌 Generic pub/sub transport
│                 │     - broadcasts, tracks, groups, frames
├─────────────────┤
│  WebTransport   │  🌐 Browser-compatible QUIC
│      QUIC       │     - HTTP/3 handshake, multiplexing, etc.
└─────────────────┘
```

## Libraries

This repository provides both [Rust](rs) and [TypeScript](js) libraries with similar APIs but language-specific optimizations.

### Rust

| Crate                       | Description                                                                                                                           | Docs                                                                           |
|-----------------------------|---------------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------|
| [moq-net](rs/moq-net)            | The networking layer: real-time pub/sub with built-in caching, fan-out, and prioritization. Negotiates either the `moq-lite` or `moq-transport` wire protocol. | [![docs.rs](https://docs.rs/moq-net/badge.svg)](https://docs.rs/moq-net)       |
| [moq-relay](rs/moq-relay)   | A clusterable relay server. This relay performs fan-out connecting multiple clients and servers together.                             |                                                                                |
| [moq-token](rs/moq-token)   | An authentication scheme supported by `moq-relay`. Can be used as a library or as [a CLI](rs/moq-token-cli) to authenticate sessions. |                                                                                |
| [moq-native](rs/moq-native) | Opinionated helpers to configure a Quinn QUIC endpoint. It's harder than it should be.                                                | [![docs.rs](https://docs.rs/moq-native/badge.svg)](https://docs.rs/moq-native) |
| [libmoq](rs/libmoq)         | C bindings for `moq-net`.                                                                                                             | [![docs.rs](https://docs.rs/libmoq/badge.svg)](https://docs.rs/libmoq)         |
| [hang](rs/hang)             | Media-specific encoding/streaming layered on top of `moq-net`. Can be used as a library.                      | [![docs.rs](https://docs.rs/hang/badge.svg)](https://docs.rs/hang)             |
| [moq-cli](rs/moq-cli)       | A CLI for publishing media to MoQ relays.                                                                                             |                                                                                |
| [moq-mux](rs/moq-mux)       | Media muxers and demuxers (fMP4/CMAF, HLS) for importing content into MoQ broadcasts.                                                 | [![docs.rs](https://docs.rs/moq-mux/badge.svg)](https://docs.rs/moq-mux)       |
| [moq-gst](rs/moq-gst)       | A GStreamer plugin for publishing or consuming MoQ broadcasts. Not built by default; requires GStreamer dev libraries.                         |                                                                                |

### TypeScript

| Package                                  | Description                                                                                                        | NPM                                                                                                   |
|------------------------------------------|--------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------|
| **[@moq/net](js/net)**             | The networking layer: real-time pub/sub with built-in caching, fan-out, and prioritization. Negotiates either the `moq-lite` or `moq-transport` wire protocol. Intended for browsers, runs server-side with a WebTransport polyfill. | [![npm](https://img.shields.io/npm/v/@moq/net)](https://www.npmjs.com/package/@moq/net)     |
| **[@moq/token](js/token)**             |  Authentication library & CLI for JS/TS environments (see [Authentication](https://doc.moq.dev/bin/relay/auth))                               | [![npm](https://img.shields.io/npm/v/@moq/token)](https://www.npmjs.com/package/@moq/token)   |
| **[@moq/hang](js/hang)**           | Core media library: catalog, container, and support. Shared by `@moq/watch` and `@moq/publish`. | [![npm](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang) |
| **[@moq/demo](demo/web)** | Examples using `@moq/hang`.                                                                                  |                                                                                                       |
| **[@moq/watch](js/watch)**         | Subscribe to and render MoQ broadcasts (Web Component + JS API).                                                        | [![npm](https://img.shields.io/npm/v/@moq/watch)](https://www.npmjs.com/package/@moq/watch)     |
| **[@moq/publish](js/publish)**     | Publish media to MoQ broadcasts (Web Component + JS API).                                                               | [![npm](https://img.shields.io/npm/v/@moq/publish)](https://www.npmjs.com/package/@moq/publish) |
| **[@moq/ui-core](js/ui-core)**     | Shared UI components (Button, Icon, Stats, CSS theme) used by `@moq/watch/ui` and `@moq/publish/ui`.                    | [![npm](https://img.shields.io/npm/v/@moq/ui-core)](https://www.npmjs.com/package/@moq/ui-core) |

## Protocol

Read the specifications:

- [moq-lite](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html)
- [hang](https://moq-dev.github.io/drafts/draft-lcurley-moq-hang.html)
- [use-cases](https://moq-dev.github.io/drafts/draft-lcurley-moq-use-cases.html)

## Development

```sh
# See all available commands
just

# Build everything
just build

# Run tests and linting
just check

# Automatically fix some linting errors
just fix
```

See the [development guide](https://doc.moq.dev/setup/dev) and the [justfile](justfile) for more.

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

**Exception:** the OBS plugin under [`cpp/obs/`](cpp/obs) is licensed under **GPL-2.0-or-later** (see [`cpp/obs/LICENSE`](cpp/obs/LICENSE)), because it links OBS Studio's `libobs`, which is GPL-2.0. This is a separately-distributable work; per GPLv2 its presence in this repository is mere aggregation and does not affect the MIT/Apache licensing of the rest of the project. `libmoq` and the other moq crates remain MIT/Apache.
