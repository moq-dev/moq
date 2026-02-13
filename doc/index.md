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
    details: Libraries for Rust (native) and TypeScript (web). Integrations with ffmpeg, OBS, GStreamer, and more to come. And C bindings!

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

[moq.dev](https://moq.dev) is an open source implementation written in Rust (native) and Typescript (web).
We support compatibility with the *official* [IETF drafts](https://datatracker.ietf.org/group/moq/documents/), but the main focus is a subset called [moq-lite](/concept/layer/moq-lite) and [hang](/concept/layer/hang).
The idea is to [build first, argue later](/concept/standard/).

See the [concepts](/concept/) page for a breakdown of the layering, rationale, and comparison to other protocols.

## Setup
Get up and running in seconds with [Nix](https://nixos.org/download.html) ([+Flakes](https://nixos.wiki/wiki/Flakes)), or be lame and [install stuff manually](/setup/):

```bash
# Runs a relay, media publisher, and the web server
nix develop -c just dev
```

If everything works, a browser window will pop up demoing how to both publish and watch content via the web.

- Keep reading the [development guide](/setup/dev) to run more advanced demos.
- Skip ahead to the [production guide](/setup/prod) to see what it takes to deploy this bad boy.

## Applications
There are a bunch of MoQ binaries and plugins.

Some highlights:
- [moq-relay](/app/relay/) - A server connecting publishers to subscribers, able to form a [self-hosted CDN cluster](/app/relay/cluster).
- [moq-cli](/app/cli) - A CLI that can import and publish MoQ broadcasts from a variety of formats (fMP4, HLS, etc), including via ffmpeg.
- [obs](/app/obs) - An OBS plugin, able to publish a MoQ broadcast and/or use MoQ broadcasts as sources.
- [gstreamer](/app/gstreamer) - A gstreamer plugin, split into a source and a sink.
- [web](/app/web) - A web component you can slap on your website to watch and publish MoQ broadcasts.
- [...and more](/app/)

## Rust Crates 🦀
Integrate MoQ into your application without fear. Focused on [native](/rs/env/native) but has token [WASM](/rs/env/wasm) support.

Some highlights:
- [moq-lite](/rs/crate/moq-lite) - Performs the core asynchronous networking, caching, and fanout.
- [moq-mux](/rs/crate/moq-mux) - Media muxers/demuxers for fMP4, CMAF, and HLS import.
- [libmoq](/rs/crate/libmoq) - C bindings for the above, no finagling Rust into your build system.
- [web-transport](/rs/crate/web-transport) - A suite of crates required to get QUIC access in the browser, plus some polyfills.
- [...and more](/rs/)

## TypeScript Packages
Run MoQ in a [web browser](/js/env/web) utilizing the latest Web tech.
Or run on [native](/js/env/native) with polyfills via Node/Bun/Deno.

Some highlights:
- [@moq/lite](/js/@moq/lite) - Performs the core asynchronous networking.
- [@moq/hang](/js/@moq/hang/) - Performs any media stuff: capture, encode, transmux, decode, render.
- [@moq/watch](/js/@moq/watch) - Subscribe to and render MoQ broadcasts.
- [@moq/publish](/js/@moq/publish) - Publish media to MoQ broadcasts.
- [...and more](/js/)
