---
title: Concepts
description: How the MoQ stack fits together
---

# Concepts

MoQ is a stack of small layers. A relay only needs the middle one, which is
what lets it forward media it can't parse.

| Layer | Role | Implemented by |
| --- | --- | --- |
| Application | Your product: rooms, auth, custom tracks. | You |
| [hang](/concept/hang) | Media: a catalog of tracks, codec config, timestamped frames. | `hang`, `@moq/hang` |
| [moq-lite](/concept/moq-lite) | Generic live pub/sub: broadcasts, tracks, groups, frames. | `moq-net`, `@moq/net`, relays, CDNs |
| [Transport](/concept/transport) | QUIC streams, via WebTransport in browsers, with a WebSocket fallback. | The browser, `moq-native` |

## The model in one paragraph

A **broadcast** is a named, discoverable collection of **tracks** from one
publisher. A **track** is a live sequence of **groups**, and each group is a
sequence of **frames** delivered reliably and in order over its own QUIC
stream (or, for a tiny single frame, as one unreliable datagram). Groups are independent: they can arrive out of order, be prioritized
against each other, and be dropped under congestion without corrupting the
rest. For video a group is a GoP starting at a keyframe; for audio it's a
second or so of samples; for data it's whatever unit can be skipped as a whole.

Subscribers pull. Nothing is transmitted until someone subscribes, duplicate
subscriptions are merged at every relay on the way upstream, and a publisher
that watches demand (the capture and transcode paths do) can skip encoding too. Each
subscriber also chooses its own latency budget, so the same broadcast can be
watched at 100 ms by one viewer and 10 s by another.

## Sections

- [Transport](/concept/transport): why QUIC, and the WebTransport, WebSocket, raw QUIC, and iroh paths onto it.
- [moq-lite](/concept/moq-lite): the pub/sub protocol, discovery, subscriptions, and congestion behavior.
- [hang](/concept/hang): the media catalog, containers, and how to extend both.
- [Standards](/concept/standard): how this relates to the IETF moq-transport, MSF, and LOC drafts.
- [Use cases](/concept/use-case/): MoQ compared with HLS/DASH, RTMP/SRT, WebRTC, and used for AI.
