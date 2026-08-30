---
layout: home

hero:
  name: Media over QUIC
  text: Live media without latency buildup
  tagline: A composable protocol stack for real-time media and data at scale.
  actions:
    - theme: brand
      text: Quick start
      link: /setup/
    - theme: alt
      text: Try the demo
      link: https://moq.dev/
    - theme: alt
      text: Understand MoQ
      link: /concept/

features:
  - icon:
      src: /emoji/rocket.svg
    title: Low latency
    details: Independent streams prevent old media from delaying the live edge during congestion.

  - icon:
      src: /emoji/stonk.svg
    title: Scalable
    details: Generic relays cache and fan out content without understanding its media format.

  - icon:
      src: /emoji/puzzle.svg
    title: Composable
    details: Protocol, media, and application layers can evolve independently and carry custom tracks.

  - icon:
      src: /emoji/globe.svg
    title: Cross-platform
    details: Native and browser implementations, language bindings, gateways, and production tools share one protocol.
---

## What is MoQ?

Media over QUIC (MoQ) is a family of protocols for live media and data. It uses
independent QUIC streams so congestion on one part of a broadcast does not block
newer content on another.

This project provides a production-oriented implementation in Rust and
TypeScript. Its primary protocol stack is [moq-lite](/concept/layer/moq-lite)
for transport and [hang](/concept/layer/hang) for media, with compatibility for
the [IETF MoQ specifications](/concept/standard/).

## Choose a path

| Goal | Start here |
| --- | --- |
| Run the local demo | [Quick start](/setup/) |
| Publish, play, or convert media | [Applications](/bin/) |
| Add MoQ to an application | [Libraries](/lib/) |
| Operate a relay | [moq-relay](/bin/relay/) |
| Learn the architecture and tradeoffs | [Concepts](/concept/) |
| Read the protocol specifications | [Internet-Drafts](/draft/) |

The main implementations are [Rust](/lib/rs/) for native applications and
[TypeScript](/lib/js/) for browsers and JavaScript runtimes. C, Python, Kotlin,
Swift, and Go bindings wrap the Rust core.

## Project links

- [Live demo](https://moq.dev/)
- [GitHub repository](https://github.com/moq-dev/moq)
- [Discord](https://discord.moq.dev)
- [IETF MoQ Working Group](https://datatracker.ietf.org/group/moq/about/)
