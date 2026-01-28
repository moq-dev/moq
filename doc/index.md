---
layout: home

hero:
  actions:
    - theme: brand
      text: Setup
      link: /setup/
    - theme: alt
      text: Usage
      link: /usage/
    - theme: alt
      text: Concepts
      link: /concepts/
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

[Media over QUIC](https://moq.dev) (MoQ) is a next-generation live media protocol that provides **real-time latency** at **massive scale**.
Built using modern web technologies, MoQ delivers WebRTC-like latency *on the web* without the constraints of WebRTC.
The core networking is delegated to QUIC while your application gets full control over the rest.

**NOTE**: This project uses [moq-lite](/concepts/moq-lite) and [hang](/concepts/hang) instead of the *official* [IETF drafts](https://datatracker.ietf.org/group/moq/documents/).
See the [IETF standards](/concept/standards) page for a justification!

## Quick Start
Get up and running in seconds with [Nix](https://nixos.org/download.html), or use an [alternative method](/setup).

```bash
# Runs a relay, media publisher, and the web server
nix develop -c just dev
```

## Usage
There are a bunch of MoQ binaries and plugins, here are some highlights:

- **[moq-relay](/usage/relay)** - A server connecting publisher to subscribers, able to form a self-hosted CDN mesh.
- **[moq-cli](/usage/cli)** - A CLI that can import and publish MoQ broadcasts from a variety of formats (fMP4, HLS, etc).
- **[moq/obs](/usage/obs)** - A dope OBS plugin for publishing and consuming MoQ broadcasts.
- **[moq/gstreamer](/usage/gstreamer)** - A dope gstreamer plugin for publishing and consuming MoQ broadcasts.

Looking for a library instead?
We have implementations in two languages:

- **[Rust](/usage/rust)** - Rust libraries primarily targetting native. 🦀
- **[Typescript](/usage/typescript)** - Typescript libraries primarily targetting web.
