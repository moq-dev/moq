---
title: moq-net
description: Real-time pub/sub with caching, fan-out, and prioritization
---

# moq-net

[![crates.io](https://img.shields.io/crates/v/moq-net)](https://crates.io/crates/moq-net)
[![docs.rs](https://docs.rs/moq-net/badge.svg)](https://docs.rs/moq-net)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

The networking layer for Media over QUIC: real-time pub/sub with built-in caching, fan-out, and prioritization, on top of QUIC. At session setup it negotiates one of two wire protocols: the simplified [moq-lite](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/) protocol or the full IETF [moq-transport](https://datatracker.ietf.org/group/moq/documents/) protocol.

> Previously published as `moq-lite`; renamed to clarify that this is the networking layer, not a specific wire protocol.

## Overview

`moq-net` provides the networking layer for MoQ, implementing broadcasts, tracks, groups, and frames on top of QUIC. Live media is built on top of this layer using something like [hang](/rs/crate/hang).

## Core Concepts

- **Broadcasts** — Discoverable collections of tracks
- **Tracks** — Named streams of data, split into groups
- **Groups** — Sequential collections of frames, independently decodable
- **Frames** — Timed chunks of data

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
moq-net = "0.1"
```

## API Reference

The Rust API uses a builder pattern with `Session`, `OriginProducer`/`OriginConsumer`, and related types. See the full API documentation for details:

**[docs.rs/moq-net](https://docs.rs/moq-net)**

## Features

- Multiple tracks can be published/subscribed simultaneously
- Groups are delivered over independent QUIC streams
- Built-in deduplication for shared subscriptions
- QUIC stream prioritization for important data
- Partial reliability, old groups can be dropped to maintain real-time latency

## Authentication

Pass JWT tokens via query parameters in the URL. See [Authentication guide](/app/relay/auth) for details.

## Protocol Specification

See the [moq-lite specification](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/) and [moq-transport drafts](https://datatracker.ietf.org/group/moq/documents/) for the wire formats that this crate speaks.

## Next Steps

- Build media apps with [hang](/rs/crate/hang)
- Use [moq-native](/rs/crate/moq-native) for QUIC/WebTransport connection helpers
- Deploy a [relay server](/app/relay/)
- Read the [Concepts guide](/concept/)
- View [code examples](https://github.com/moq-dev/moq/tree/main/rs/moq-native/examples)
