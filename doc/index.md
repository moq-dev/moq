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

Media over QUIC (MoQ) is a live media protocol built on QUIC. A publisher
sends a **broadcast** made of **tracks**; each track is a series of **groups**
(a group of pictures, a second of audio, one JSON snapshot). Most groups use
independent QUIC streams; eligible single-frame groups can use unreliable
datagrams. Relays forward and cache the stream-delivered groups without parsing
them, so the same infrastructure carries video, audio, and arbitrary data.

This project is the reference implementation: a Rust relay and toolchain, a
TypeScript browser stack, and bindings for C, Python, Kotlin, Swift, Go, and
Dart. It speaks [moq-lite](/concept/moq-lite), a simple profile that is
forward-compatible with the IETF [moq-transport](/concept/standard) drafts.

## What you can build

| Use case | Reach for |
| --- | --- |
| Twitch-style live streaming | Ingest with [OBS](/bin/obs), [RTMP or SRT](/bin/cli), distribute with [moq-relay](/bin/relay/), watch with [`<moq-watch>`](/lib/js/watch), and keep legacy players via the [HLS gateway](/bin/hls). |
| Conferencing | [`<moq-publish>`](/lib/js/publish) and [`<moq-watch>`](/lib/js/watch) in the browser, one broadcast per participant, plus the [WebRTC gateway](/bin/rtc) for WHIP/WHEP clients. |
| Voice and video AI | Server-side media in [Rust](/lib/rs/) or [Python](/lib/py/) with hardware codecs, faster-than-real-time playback in the browser. See [MoQ for AI](/concept/use-case/ai). |
| Real-time data | Chat, game state, telemetry, and control channels over the same relays with [`moq-net`](/lib/rs/moq-net) or [`@moq/net`](/lib/js/net). |
| Interactive streams | Media down, input up. [MoQ Boy](/bin/demo) is a crowd-controlled Game Boy built this way. |
| Native and mobile apps | One Rust core behind [Swift](/lib/swift/), [Kotlin](/lib/kt/), [Go](/lib/go/), [Dart](/lib/dart/), and [C](/lib/c/) bindings. |

## Choose a path

| Goal | Start here |
| --- | --- |
| Run the demo locally | [Quick start](/setup/) |
| Install the relay or CLI | [Install](/setup/install) |
| Publish, play, or convert media | [Applications](/bin/) |
| Add MoQ to an app | [Libraries](/lib/) |
| Operate a relay | [moq-relay](/bin/relay/) |
| Understand the design | [Concepts](/concept/) |
| Read the specs | [Internet-Drafts](/draft/) |
| Teach your coding agent | [Agent setup](/setup/agent) |

## Project links

- [Live demo](https://moq.dev/) and [blog](https://moq.dev/blog)
- [GitHub](https://github.com/moq-dev/moq)
- [Discord](https://discord.moq.dev)
- [IETF MoQ Working Group](https://datatracker.ietf.org/group/moq/about/)
