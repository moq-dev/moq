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
      link: /app/
    - theme: alt
      text: Rust
      link: /rs/
    - theme: alt
      text: Typescript
      link: /js/
    - theme: alt
      text: Demo
      link: https://moq.dev/

features:
  - icon: 🚀
    title: Latency
    details: MoQ supports the entire latency spectrum on a per-viewer basis. Support real-time, interactive, or sit-back user experiences with a unified stack.

  - icon: 📈
    title: Massive Scale
    details: All content can be cached and fanned-out via a generic CDN. Serve millions of concurrent viewers across the globe, including via Cloudflare (and more soon).

  - icon: 🔧
    title: Flexible
    details: Supports contribution, distribution, and even conferencing. Extend the protocol with custom tracks for any sort of live content.

  - icon: 🌐
    title: Modern Web
    details: Utilizes WebTransport, WebCodecs, and WebAudio APIs for native browser compatibility without hacks.

  - icon: 🎯
    title: Multi-platform
    details: Libraries in Rust (native) and TypeScript (web). Integrations for ffmpeg, OBS, Gstreamer, and more to come.

  - icon: 💪
    title: Efficient
    details: Save resources by only encoding or transmitting data when needed. Built on top of production-ready QUIC libraries.

  - icon: 🔒
    title: Secure
    details: Encrypted via TLS and authenticated via JWT. You can optionally self-host a private CDN or end-to-end encrypt your content.

  - icon: ⏪
    title: Backwards Compatible
    details: Supports CMAF and HLS for legacy device support. Gradually migrate your users without breaking a leg.

  - icon: 👯
    title: Peer-to-Peer
    details: Utilize a CDN and/or connect directly to peers via Iroh (native only). Automatically discovers broadcasts available on a connection.
---

## What is MoQ?

**Media over QUIC** (MoQ) is a next-generation live media protocol.
As the name implies, it leverages QUIC to transmit media across multiple concurrent streams, potentially out-of-order.
The protocol is being standardized by the [IETF](https://datatracker.ietf.org/group/moq/), backed by some of the largest tech companies: Google, Cisco, Akamai, Cloudflare, etc.

[moq.dev](https://moq.dev) is an open source implementation written in Rust (native) and Typescript (web).
We support compatibility with the *official* [IETF drafts](https://datatracker.ietf.org/group/moq/documents/), but the main focus is a subset called [moq-lite](/concept/layer/moq-lite) and [hang](/concept/layer/hang).
The idea is to [build first, argue later](/concept/standard/).

See the [concepts](/concept/) page for a breakdown of the layering, rationale, and comparison to other protocols.

## Setup
Get up and running in seconds with [Nix](https://nixos.org/download.html), or be lame and [install stuff manually](/setup/):

```bash
# Runs a relay, media publisher, and the web server
nix develop -c just dev
```

If everything works, a browser window will pop up demoing how to both publish and watch content via the web.

- Keep reading the [development guide](/setup/dev) to run more advanced demos.
- Skip ahead to the [production guide](/setup/prod) to see what it takes to run this bad boy.

## Applications
There are a bunch of MoQ binaries and plugins. Some highlights:

- [moq-relay](/app/relay/) - A server connecting publishers to subscribers, able to form a [self-hosted CDN cluster](/app/relay/cluster).
- [hang-cli](/app/cli) - A CLI that can import and publish MoQ broadcasts from a variety of formats (fMP4, HLS, etc), including via ffmpeg.
- [obs](/app/obs) - An OBS plugin, able to publish a MoQ broadcast and/or use MoQ broadcasts as sources.
- [gstreamer](/app/gstreamer) - A gstreamer plugin, split into a source and a sink.
- [web](/app/web) - A web component you can throw on your website, wrapping a `<video>` tag. It can also do publishing!
- [...and more](/app/)

## Rust Crates 🦀
Integrate MoQ into your application without fear.
The main focus is [native](/rs/env/native), but there's also some [WASM support](/rs/env/wasm).

Some highlights:
- [moq-lite](/rs/crate/moq-lite) - Performs the core asynchronous networking, caching, and fanout.
- [hang](/rs/crate/hang) - Performs any media stuff: currently only transmuxing.
- [libmoq](/rs/crate/libmoq) - C bindings for the above, no finagling Rust into your build system.
- [web-transport](/rs/crate/web-transport) - A suite of crates required to get QUIC access in the browser, plus some polyfills.
- [...and more](/rs/)

## TypeScript Packages
Run MoQ in a [web browser](/js/env/web) utilizing the latest Web tech.
Or run a [native app](/js/env/native) (with polyfills) via Node/Bun/Deno.

Some highlights:
- [@moq/lite](/js/@moq/lite) - Performs the core asynchronous networking.
- [@moq/hang](/js/@moq/hang/) - Performs any media stuff: capture, encode, transmux, decode, render.
- [@moq/hang-ui](/js/@moq/hang-ui) - A simple web UI for those too lazy to vibe code one.
- [...and more](/js/)
