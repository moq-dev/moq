---
title: Repository map
description: Find the implementation, examples, tests, and reference docs for a MoQ feature
---

# Repository map

Use this page when changing MoQ itself. To build an application, start with
[Libraries](/lib/) and [Examples](/setup/examples). For build commands and the
local feedback loop, see [Development](/setup/dev).

## Find the implementation

Paths below are relative to the [repository root](https://github.com/moq-dev/moq).
Rust and TypeScript implement the protocol independently; the other languages
wrap the Rust core. [Layers](/lib/layers) maps the binding namespaces.

| Working on | Start here | Orientation |
| --- | --- | --- |
| Broadcast discovery, subscriptions, caching, groups, frames | [`rs/moq-net`](https://github.com/moq-dev/moq/tree/main/rs/moq-net), [`js/net`](https://github.com/moq-dev/moq/tree/main/js/net) | [moq-lite](/concept/moq-lite) |
| Congestion controllers and controller tests | [`rs/moq-congestion`](https://github.com/moq-dev/moq/tree/main/rs/moq-congestion) | [Transport](/concept/transport) |
| Connecting, listening, TLS, transport fallback | [`rs/moq-tokio`](https://github.com/moq-dev/moq/tree/main/rs/moq-tokio), [`js/net/src/connection`](https://github.com/moq-dev/moq/tree/main/js/net/src/connection) | [Transport](/concept/transport) |
| Relay routing, clustering, HTTP, configuration | [`rs/moq-relay`](https://github.com/moq-dev/moq/tree/main/rs/moq-relay) | [Relay](/bin/relay/) |
| Authorization and path permissions | [`rs/moq-auth`](https://github.com/moq-dev/moq/tree/main/rs/moq-auth), [`js/auth`](https://github.com/moq-dev/moq/tree/main/js/auth) | [Authentication](/bin/relay/auth) |
| Media catalogs and frame containers | [`rs/hang`](https://github.com/moq-dev/moq/tree/main/rs/hang), [`js/hang`](https://github.com/moq-dev/moq/tree/main/js/hang) | [hang](/concept/hang) |
| Importing and exporting media formats | [`rs/moq-mux`](https://github.com/moq-dev/moq/tree/main/rs/moq-mux) | [Muxing](/lib/rs/moq-mux) |
| Browser capture, playback, and controls | [`js/publish`](https://github.com/moq-dev/moq/tree/main/js/publish), [`js/watch`](https://github.com/moq-dev/moq/tree/main/js/watch), [`js/video`](https://github.com/moq-dev/moq/tree/main/js/video), [`js/ui`](https://github.com/moq-dev/moq/tree/main/js/ui) | [TypeScript](/lib/js/) |
| Browser reactive state and cleanup | [`js/signals`](https://github.com/moq-dev/moq/tree/main/js/signals) | [Signals](/lib/js/signals) |
| Native capture, codecs, and rendering | [`rs/moq-audio`](https://github.com/moq-dev/moq/tree/main/rs/moq-audio), [`rs/moq-video`](https://github.com/moq-dev/moq/tree/main/rs/moq-video), [`rs/moq-transcode`](https://github.com/moq-dev/moq/tree/main/rs/moq-transcode) | [Audio](/lib/rs/moq-audio), [video](/lib/rs/moq-video) |
| The native studio: scenes, compositing, and its window | [`rs/moq-scene`](https://github.com/moq-dev/moq/tree/main/rs/moq-scene), [`rs/moq-studio`](https://github.com/moq-dev/moq/tree/main/rs/moq-studio) | [Studio](/bin/studio) |
| CLI commands and protocol gateways | [`rs/moq-cli`](https://github.com/moq-dev/moq/tree/main/rs/moq-cli), then the corresponding `rs/moq-{rtmp,srt,rtc,rtsp,hls}` crate | [Applications](/bin/) |
| Language bindings | [`rs/moq-ffi`](https://github.com/moq-dev/moq/tree/main/rs/moq-ffi), [`rs/moq-c`](https://github.com/moq-dev/moq/tree/main/rs/moq-c), then `py/`, `swift/`, `kt/`, `go/`, `dart/`, or `cpp/moq/` | [Binding layers](/lib/layers) |
| Rooms, chat, and application data | [`rs/moq-room`](https://github.com/moq-dev/moq/tree/main/rs/moq-room), [`js/room`](https://github.com/moq-dev/moq/tree/main/js/room), `rs/moq-json`, `js/json` | [Rooms](/lib/js/room), [JSON tracks](/lib/js/json) |
| End-to-end encryption | [`rs/moq-e2ee`](https://github.com/moq-dev/moq/tree/main/rs/moq-e2ee), [`js/e2ee`](https://github.com/moq-dev/moq/tree/main/js/e2ee) | [Media encryption](/concept/hang), [E2EE draft](/draft/moq-e2ee) |
| Signed content | [`rs/moq-sign`](https://github.com/moq-dev/moq/tree/main/rs/moq-sign), [`js/sign`](https://github.com/moq-dev/moq/tree/main/js/sign) | [Signing draft](/draft/moq-sign) |
| Async primitives and deterministic time | [`rs/kio`](https://github.com/moq-dev/moq/tree/main/rs/kio), [`rs/moq-clock`](https://github.com/moq-dev/moq/tree/main/rs/moq-clock), [`js/clock`](https://github.com/moq-dev/moq/tree/main/js/clock) | [Rust conventions](/lib/rs/#conventions), [clock](/lib/js/clock) |

## Find the surrounding work

| Need | Where to look |
| --- | --- |
| A runnable consumer | [Examples](/setup/examples), [`demo/`](https://github.com/moq-dev/moq/tree/main/demo) |
| Protocol and format definitions | [`drafts/`](https://github.com/moq-dev/moq/tree/main/drafts), rendered as [Internet-Drafts](/draft/) |
| Cross-language and browser tests | [`test/README.md`](https://github.com/moq-dev/moq/blob/main/test/README.md) and [`test/justfile`](https://github.com/moq-dev/moq/blob/main/test/justfile) |
| Performance measurements | [`bench/README.md`](https://github.com/moq-dev/moq/blob/main/bench/README.md), `rs/*/benches/`, `js/*/bench/` |
| Build and check selection | [`justfile`](https://github.com/moq-dev/moq/blob/main/justfile), language justfiles, [`sh/dispatch.sh`](https://github.com/moq-dev/moq/blob/main/sh/dispatch.sh) |
| CI and packaging | [`.github/workflows/`](https://github.com/moq-dev/moq/tree/main/.github/workflows), [`infra/`](https://github.com/moq-dev/moq/tree/main/infra), [`sh/`](https://github.com/moq-dev/moq/tree/main/sh) |
| Generated binding ownership | [`rs/moq-ffi/AGENTS.md`](https://github.com/moq-dev/moq/blob/main/rs/moq-ffi/AGENTS.md), [FFI release layout](https://github.com/moq-dev/moq/blob/main/.github/ffi.md) |
| Contribution and release workflow | [`CONTRIBUTING.md`](https://github.com/moq-dev/moq/blob/main/CONTRIBUTING.md) |

## Read the right version

`main` is the development trunk; `release` is what ships. A package installed
from a registry may differ from the source linked here. For application work,
use the API reference and examples matching the installed version. For a
repository change, use this checkout's source and `doc/` pages. See
[Upgrade](/setup/upgrade) for migration guides.

This site explains what to choose and where to start. Exact signatures live
in Rustdoc and TSDoc next to the source, command options in `--help`, and wire
formats in `drafts/`. The [Rust](/lib/rs/) and [TypeScript](/lib/js/) indexes
lead to package references.

## Working with an agent

Start with the root [`AGENTS.md`](https://github.com/moq-dev/moq/blob/main/AGENTS.md)
and the nested instructions along the path being changed. Its cross-package
table identifies the other implementations, bindings, and docs affected by a
change. See [Coding agents](/setup/agent#working-in-this-repository) for the
repository workflow; installing the application-building skill is unnecessary
for a checkout that already contains these instructions.
