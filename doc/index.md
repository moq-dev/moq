---
layout: home

hero:
  name: Media over QUIC
  text: Live media without latency buildup
  tagline: Real-time latency at CDN scale, for video, audio, and any live data.
  actions:
    - theme: brand
      text: Quick start
      link: /setup/
    - theme: alt
      text: Try the demo
      link: https://moq.dev/
    - theme: alt
      text: How it works
      link: /concept/

features:
  - icon:
      src: /emoji/rocket.svg
    title: Low latency
    details: Independent QUIC streams let congestion drop old media instead of delaying new media, while tiny single-frame groups can use datagrams.

  - icon:
      src: /emoji/stonk.svg
    title: Scalable
    details: Relays cache and fan out broadcasts without understanding the payload, and cluster across regions.

  - icon:
      src: /emoji/puzzle.svg
    title: Composable
    details: Generic pub/sub underneath, media on top. Add your own tracks for chat, input, telemetry, or AI output.

  - icon:
      src: /emoji/globe.svg
    title: Everywhere
    details: Browsers via WebTransport, native apps via Rust and language bindings, plus gateways to RTMP, SRT, HLS, and WebRTC.
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
