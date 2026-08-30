---
title: Concepts
description: Architecture, standards, and use cases for MoQ
---

# Concepts

MoQ separates network transport, live pub/sub, media packaging, and application
behavior. That separation lets relays remain media-agnostic while applications
choose the formats and features they need.

## [Layers](/concept/layer/)

How QUIC, WebTransport, moq-lite, hang, and application tracks fit together.
Start here if you are new to the protocol or deciding which library layer to use.

## [Standards](/concept/standard/)

How this implementation relates to the IETF MoQ work, including MoqTransport,
MSF, LOC, and the specifications maintained by moq.dev.

## [Use cases](/concept/use-case/)

The requirements and tradeoffs for contribution, distribution, conferencing,
and real-time AI workloads.
