---
title: Concepts
description: Understanding MoQ's fundamental concepts
---

# Concepts
Welcome to my favorite section.
MoQ has been a multi-year journey to solve some very real problems in the industry and now it's time to flex the design.

## Layers

The design philosophy of MoQ is to make things simple, composable, and customizable.
We don't want you to hit a brick wall if you deviate from the standard path (*ahem* WebRTC).
We also want to benefit from economies of scale (like HTTP), utilizing generic libraries and tools whenever possible.

To accomplish this, MoQ is broken into layers:

```text
┌─────────────────┐
│   Application   │   🏢 Your business logic
│                 │    - authentication, non-media tracks, etc.
├─────────────────┤
│  Media Format   │   🎬 Media-specific encoding/streaming
│     (hang)      │     - codecs, containers, catalog
├─────────────────├
│  MoQ Transport  │  🚌 Generic pub/sub transport
│   (moq-lite)    │     - broadcasts, tracks, groups, frames
├─────────────────┤
│  WebTransport   │  🌐 Browser-compatible QUIC
│                 │     - HTTP/3 handshake
├─────────────────┤
|      QUIC       |  🌐 Underlying transport protocol
│                 │     - streams, datagrams, prioritization, etc.
└─────────────────┘
```

You get to choose which layers you want to use and which layers you want to replace.
It's like a cake but reusable.

See [Layers](/concept/layer/) for more information.

## Standards
MoQ is built on open standards and protocol specifications.
We're in this together, even if we disagree on some details.

See [Standards](/concept/standard/) for more information.

## Use Cases
MoQ is designed to be used in a variety of use-cases.
Distribution, contribution, conferencing, and more.

See [Use Cases](/concept/use-case/) for more information.
