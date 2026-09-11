---
layout: home

hero:
  actions:
    - theme: brand
      text: Setup
      link: /setup/
    - theme: alt
      text: Concepts
      link: /concept/
    - theme: alt
      text: Apps
      link: /bin/
    - theme: alt
      text: Libraries
      link: /lib/
    - theme: alt
      text: Demo
      link: https://moq.dev/

features:
  - icon:
      src: /emoji/rocket.svg
    title: Adaptive
    details: MoQ supports the entire latency spectrum. Simultaneously support real-time, interactive, or lean-back experiences with a unified stack.

  - icon:
      src: /emoji/stonk.svg
    title: Scalable
    details: All content can be cached and fanned-out via a CDN. Serve millions of concurrent viewers across the globe, including via Cloudflare.

  - icon:
      src: /emoji/puzzle.svg
    title: Extensible
    details: Supports contribution, distribution, conferencing, and whatever you can dream up. Extend the protocol with custom tracks for any live content.

  - icon:
      src: /emoji/globe.svg
    title: Modern Web
    details: Utilizes WebTransport, WebCodecs, and WebAudio APIs for modern browser support without hacks.

  - icon:
      src: /emoji/box.svg
    title: Cross-Platform
    details: Libraries for Rust (native) and TypeScript (web), plus FFI bindings for C, Python, Kotlin, Swift, Go, and Dart. Integrations with ffmpeg, OBS, GStreamer, and more to come.

  - icon:
      src: /emoji/battery.svg
    title: Efficient
    details: Save resources by only encoding or transmitting data when needed. Built on top of production-ready QUIC libraries.

  - icon:
      src: /emoji/lock.svg
    title: Secure
    details: Encrypted via TLS and authenticated via JWT. You can optionally self-host a private CDN or end-to-end encrypt your content.

  - icon:
      src: /emoji/back.svg
    title: Backwards Compatible
    details: Supports CMAF and HLS for legacy device support. Migrate legacy devices at your own pace.

  - icon:
      src: /emoji/link.svg
    title: Decentralized
    details: Host your own CDN, use a 3rd party service, and/or connect P2P via Iroh (native only). Broadcasts are automatically discovered and gossiped.
---


## What is MoQ?

**Media over QUIC** (MoQ) is a next-generation live media protocol.
As the name implies, we use QUIC to concurrently transmit media and avoid latency build-up during congestion.
The protocol is being standardized by the [IETF](https://datatracker.ietf.org/group/moq/about/) and backed by some of the largest tech companies: Google, Cisco, Akamai, Cloudflare, etc.

[moq.dev](https://moq.dev) is an open source implementation written in Rust (native) and TypeScript (web).
We support compatibility with the *official* [IETF drafts](/draft/), but the main focus is a subset called [moq-lite](/concept/moq-lite) and [hang](/concept/hang).
The idea is to [build first, argue later](/concept/standard).

See the [concepts](/concept/) page for a breakdown of the layering, rationale, and comparison to other protocols.

## What you can build

| Use case | Reach for |
| --- | --- |
| Live streaming | Ingest with [OBS](/bin/obs), [RTMP](/bin/rtmp), or [SRT](/bin/srt); distribute with [moq-relay](/bin/relay/); watch with [`<moq-watch>`](/lib/js/watch); keep legacy players via [HLS](/bin/hls). |
| Conferencing | [`<moq-publish>`](/lib/js/publish) and [`<moq-watch>`](/lib/js/watch) in the browser, one broadcast per participant, plus [WebRTC](/bin/rtc) for WHIP/WHEP clients. |
| Voice and video AI | Server-side media in [Rust](/lib/rs/) or [Python](/lib/py/), faster-than-real-time playback in the browser. See [MoQ for AI](/concept/use-case/ai). |
| Real-time data | Chat, game state, telemetry, and control channels over the same relays with [`moq-net`](/lib/rs/moq-net) or [`@moq/net`](/lib/js/net). |
| Interactive streams | Media down, input up. [MoQ Boy](/bin/demo) is a crowd-controlled Game Boy built this way. |

## Setup

Get up and running in seconds with [Nix](https://nixos.org/download.html) ([+Flakes](https://nixos.wiki/wiki/Flakes)), or be lame and [install stuff manually](/setup/install):

```bash
# Runs a relay, media publisher, and the web server
nix develop -c just
```

If everything works, a browser window will pop up demoing how to both publish and watch content via the web.

- Keep reading the [development guide](/setup/dev) to run more advanced demos.
- Skip ahead to the [production guide](/setup/prod) to see what it takes to deploy this bad boy.
- Using an AI coding agent? [Teach it MoQ](/setup/agent) with a one-line prompt.

## Applications

There are a bunch of MoQ binaries and plugins.

Some highlights:

- [moq-relay](/bin/relay/) - A server connecting publishers to subscribers, able to form a [self-hosted CDN cluster](/bin/relay/cluster).
- [moq-cli](/bin/cli) - A CLI that can import and publish MoQ broadcasts from a variety of formats (fMP4, HLS, MPEG-TS, FLV, etc), including via ffmpeg.
- [obs](/bin/obs) - An OBS plugin, able to publish a MoQ broadcast and/or use MoQ broadcasts as sources.
- [gstreamer](/bin/gstreamer) - A gstreamer plugin, split into a source and a sink.
- [rtmp](/bin/rtmp), [srt](/bin/srt), [rtc](/bin/rtc), [hls](/bin/hls) - Gateways to and from the protocols your existing tools speak.
- [...and more](/bin/)

## Rust Crates 🦀

Integrate MoQ into your application without fear. Focused on native but with [WASM](/lib/rs/#webassembly) support.

Some highlights:

- [moq-net](/lib/rs/moq-net) - Real-time pub/sub with built-in caching, fan-out, and prioritization.
- [hang](/lib/rs/hang) - The media catalog and container on top of moq-net.
- [moq-mux](/lib/rs/moq-mux) - Media muxers/demuxers for fMP4, CMAF, MPEG-TS, and FLV.
- [moq-video](/lib/rs/moq-video) and [moq-audio](/lib/rs/moq-audio) - Native capture, encode, decode, and render with hardware codecs.
- [...and more](/lib/rs/)

## TypeScript Packages

Run MoQ in a web browser utilizing the latest Web tech.
Or run on native with polyfills via Node/Bun/Deno.

Some highlights:

- [@moq/net](/lib/js/net) - Real-time pub/sub with built-in caching, fan-out, and prioritization.
- [@moq/hang](/lib/js/hang) - Performs any media stuff: capture, encode, transmux, decode, render.
- [@moq/watch](/lib/js/watch) - Subscribe to and render MoQ broadcasts, with an optional `<moq-watch>` UI.
- [@moq/publish](/lib/js/publish) - Publish media to MoQ broadcasts, with an optional `<moq-publish>` UI.
- [...and more](/lib/js/)

## Other Languages

FFI bindings around the Rust core, with idiomatic APIs in each language:

- [C](/lib/c/) - `libmoq` static + shared library with an auto-generated header.
- [Python](/lib/py/) - `asyncio`-friendly bindings, published to PyPI.
- [Kotlin](/lib/kt/) - Coroutines and `Flow` for Android and the JVM.
- [Swift](/lib/swift/) - Async sequences for iOS, iPadOS, and macOS.
- [Go](/lib/go/) - cgo bindings resolved via `go get`.
- [Dart](/lib/dart/) - Futures and streams for Flutter and the Dart VM.
