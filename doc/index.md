---
layout: home

hero:
  actions:
    - theme: brand
      text: Setup
      link: /setup/
    - theme: alt
      text: Concepts
      link: /concepts/
    - theme: alt
      text: API
      link: /api/
    - theme: alt
      text: Demo
      link: https://moq.dev/

features:
  - icon: 🚀
    title: Real-time Latency
    details: MoQ supports the entire latency spectrum, down to the tens of milliseconds. All thanks to QUIC.

  - icon: 📈
    title: Massive Scale
    details: Everything is designed to fan-out across a generic CDN. Able to handle millions of concurrent viewers across the globe.

  - icon: 🌐
    title: Modern Web
    details: Uses WebTransport, WebCodecs, and WebAudio APIs for native browser compatibility without hacks.

  - icon: 🎯
    title: Multi-platform
    details: Implemented in Rust (native) and TypeScript (web). Comes with integrations for ffmpeg, OBS, Gstreamer, and more to come.

  - icon: 🔧
    title: Generic Protocol
    details: Not just for media; MoQ is able to deliver any live or custom data. Your application is in control.

  - icon: 💪
    title: Efficient
    details: Save resources by only encoding or transmitting data when needed. Built on top of production-grade QUIC libraries.
---

## What is MoQ?

[Media over QUIC](https://moq.dev) (MoQ) is a next-generation live media protocol that provides **real-time latency** at **massive scale**. Built using modern web technologies, MoQ delivers WebRTC-like latency without the constraints of WebRTC. The core networking is delegated to a QUIC library but the rest is in application-space, giving you full control over your media pipeline.

**NOTE**: This project uses [moq-lite](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/) and [hang](https://datatracker.ietf.org/doc/draft-lcurley-moq-hang/) instead of the *official* [IETF drafts](https://datatracker.ietf.org/group/moq/documents/).
The focus is on simplicity and deployability, avoiding the bloat and politics experimental protocols designed by committee.
We support compatibility with a subset of the latest IETF drafts, but it's *not recommended* given the ongoing standardization churn.

## Quick Start

Get up and running in seconds with [Nix](https://nixos.org/download.html), or use an [alternative method](/setup).

```bash
# Runs a relay, media publisher, and the web server
nix develop -c just dev
```

## Rust
The Rust libraries are intended for native platforms, such as desktop applications or servers.

- **[moq-lite](/rust/moq-lite)** - The core pub/sub transport protocol; media agnostic.
- **[moq-relay](/rust/moq-relay)** - A clusterable relay server that can form a CDN.
- **[hang](/rust/hang)** - The media library: provides codecs, containers, etc.
- **[hang-cli](/rust/hang-cli)** - A CLI tool for publishing media from a variety of sources.

[Full Rust Documentation →](/rust/)

## TypeScript
The TypeScript libraries are intended for web browsers, but also work server-side with a [WebTransport polyfill](https://github.com/fails-components/webtransport).

- **[@moq/lite](/typescript/lite)** - The core pub/sub transport protocol; media agnostic.
- **[@moq/hang](/typescript/hang)** - The media library; provides codecs, containers, etc.
- **[@moq/hang-ui](https://www.npmjs.com/package/@moq/hang-ui)** - Optional UI controls layered on top of `@moq/hang`.

[Full TypeScript Documentation →](/typescript/)
