---
title: moq-lite
description: Core pub/sub transport protocol in Rust
---

# moq-lite

[![crates.io](https://img.shields.io/crates/v/moq-lite)](https://crates.io/crates/moq-lite)
[![docs.rs](https://docs.rs/moq-lite/badge.svg)](https://docs.rs/moq-lite)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

The core pub/sub transport protocol implementing the [moq-lite specification](/spec/draft-lcurley-moq-lite).

## Overview

`moq-lite` provides the networking layer for MoQ, implementing broadcasts, tracks, groups, and frames on top of QUIC. Live media is built on top of this layer using something like [hang](/rs/crate/hang).

## Core Concepts

- **Broadcasts** — Discoverable collections of tracks
- **Tracks** — Named streams of data, split into groups
- **Groups** — Sequential collections of frames, independently decodable
- **Frames** — Timed chunks of data

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
moq-lite = "0.1"
```

## API Reference

The Rust API uses a builder pattern with `Session`, `OriginProducer`/`OriginConsumer`, and related types. See the full API documentation for details:

**[docs.rs/moq-lite](https://docs.rs/moq-lite)**

## Features

- Multiple tracks can be published/subscribed simultaneously
- Groups are delivered over independent QUIC streams
- Built-in deduplication for shared subscriptions
- QUIC stream prioritization for important data
- Partial reliability — old groups can be dropped to maintain real-time latency

## Authentication

Pass JWT tokens via query parameters:

```rust
let url = format!("https://relay.example.com/demo?jwt={}", token);
```

See [Authentication guide](/app/relay/auth) for details.

## Protocol Specification

See the [moq-lite specification](/spec/draft-lcurley-moq-lite) for protocol details.

## Next Steps

- Build media apps with [hang](/rs/crate/hang)
- Use [moq-native](/rs/crate/moq-native) for QUIC/WebTransport connection helpers
- Deploy a [relay server](/app/relay/)
- Read the [Concepts guide](/concept/)
- View [code examples](https://github.com/moq-dev/moq/tree/main/rs)
