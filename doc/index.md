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
    details: Host your own CDN and/or use a 3rd party service. Broadcasts are automatically discovered and gossiped.
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
| Conferencing | [`@moq/room`](/lib/js/room) / [`moq-room`](/lib/rs/moq-room) for the roster, [`<moq-publish>`](/lib/js/publish) and [`<moq-watch>`](/lib/js/watch) in the browser, plus [WebRTC](/bin/rtc) for WHIP/WHEP clients. |
| Voice and video AI | Server-side media in [Rust](/lib/rs/) or [Python](/lib/py/), faster-than-real-time playback in the browser. See [MoQ for AI](/concept/use-case/ai). |
| Real-time data | Chat, game state, telemetry, and control channels over the same relays with [`moq-net`](/lib/rs/moq-net) or [`@moq/net`](/lib/js/net). |
| Interactive streams | Media down, input up. [MoQ Boy](/bin/demo) is a crowd-controlled Game Boy built this way. |

## Start here

| Goal | Next step |
| --- | --- |
| Try MoQ locally | [Quick start](/setup/) starts a relay, publisher, and web player. |
| Add MoQ to an application | [Choose a library](/lib/) and find an [example](/setup/examples) to adapt. |
| Use existing media tools | [Applications](/bin/) covers the CLI, OBS, GStreamer, and gateways. |
| Operate a relay | [Install](/setup/install), then [deploy](/setup/prod); [inspect](/bin/inspect) it when running. |
| Understand a feature | [Concepts](/concept/) explains the layers and links to the [drafts](/draft/). |
| Diagnose a problem | [Troubleshooting](/setup/troubleshooting) points to the relevant diagnostics. |
| Contribute to MoQ | [Repository map](/setup/repository) locates source and tests; [Development](/setup/dev) covers the workflow. |
| Use a coding agent | [Agent setup](/setup/agent) covers application and repository work. [llms.txt](/llms.txt) indexes the markdown docs. |
