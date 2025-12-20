---
title: moq-lite
description: Core pub/sub transport protocol in Rust
---

# moq-lite

[![crates.io](https://img.shields.io/crates/v/moq-lite)](https://crates.io/crates/moq-lite)
[![docs.rs](https://docs.rs/moq-lite/badge.svg)](https://docs.rs/moq-lite)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

The core pub/sub transport protocol implementing the [moq-lite specification](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html).

## Overview

`moq-lite` provides the networking layer for MoQ, implementing broadcasts, tracks, groups, and frames on top of QUIC. Live media is built on top of this layer using something like [hang](/rust/hang).

## Core Concepts

- **Broadcasts** - Discoverable collections of tracks
- **Tracks** - Named streams of data, split into groups
- **Groups** - Sequential collections of frames, independently decodable
- **Frames** - Timed chunks of data

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
moq-lite = "0.1"
```

## Quick Start

### Publishing

```rust
use moq_lite::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to a relay
    let connection = Connection::connect("https://relay.example.com/demo").await?;

    // Create a broadcast
    let mut broadcast = BroadcastProducer::new("my-broadcast");

    // Create a track
    let mut track = broadcast.create_track("chat");

    // Append a group with frames
    let mut group = track.append_group();
    group.write(b"Hello, MoQ!")?;
    group.write(b"Second message")?;
    group.close()?;

    // Publish to the relay
    connection.publish(&mut broadcast).await?;

    Ok(())
}
```

### Subscribing

```rust
use moq_lite::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to a relay
    let connection = Connection::connect("https://relay.example.com/demo").await?;

    // Subscribe to a broadcast
    let broadcast = connection.consume("my-broadcast").await?;

    // Subscribe to a specific track
    let mut track = broadcast.subscribe("chat").await?;

    // Read groups and frames
    while let Some(group) = track.next_group().await? {
        println!("New group: {}", group.id());

        while let Some(frame) = group.read().await? {
            println!("Frame: {:?}", frame);
        }
    }

    Ok(())
}
```

### Track Discovery

```rust
use moq_lite::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::connect("https://relay.example.com/demo").await?;

    // Wait for announcements
    while let Some(announcement) = connection.announced().await? {
        println!("New broadcast: {}", announcement.name);

        // Subscribe to the broadcast
        let broadcast = connection.consume(&announcement.name).await?;

        // Process tracks...
    }

    Ok(())
}
```

## Features

### Concurrency

`moq-lite` is designed for concurrent use:

- Multiple tracks can be published/subscribed simultaneously
- Groups are delivered over independent QUIC streams
- Built-in deduplication for shared subscriptions

### Prioritization

Groups can be prioritized:

```rust
let mut group = track.append_group();
group.set_priority(10); // Higher priority
group.write(keyframe_data)?;
```

This leverages QUIC's stream prioritization to send important data first.

### Partial Reliability

Old groups can be dropped when behind:

```rust
let mut group = track.append_group();
group.set_expires(Duration::from_secs(2)); // Drop if not delivered in 2s
```

This maintains real-time latency by skipping stale data.

## Connection Management

### TLS Configuration

```rust
use moq_lite::*;

let mut config = ConnectionConfig::default();
config.set_verify_certificate(true);

let connection = Connection::connect_with_config(
    "https://relay.example.com/demo",
    config
).await?;
```

### Authentication

Pass JWT tokens via query parameters:

```rust
let url = format!("https://relay.example.com/demo?jwt={}", token);
let connection = Connection::connect(&url).await?;
```

See [Authentication guide](/guide/authentication) for details.

## Error Handling

```rust
use moq_lite::*;

match connection.publish(&mut broadcast).await {
    Ok(()) => println!("Published successfully"),
    Err(Error::ConnectionClosed) => println!("Connection closed"),
    Err(Error::InvalidPath(path)) => println!("Invalid path: {}", path),
    Err(e) => println!("Other error: {}", e),
}
```

## Advanced Usage

### Custom Transport

Use your own QUIC implementation:

```rust
use moq_lite::*;
use quinn::Connection as QuinnConnection;

let quinn_conn = /* your Quinn connection */;
let connection = Connection::from_quic(quinn_conn);
```

### Metadata

Attach metadata to broadcasts and tracks:

```rust
let mut broadcast = BroadcastProducer::new("my-broadcast");
broadcast.set_metadata("description", "My awesome broadcast");

let mut track = broadcast.create_track("video");
track.set_metadata("codec", "h264");
```

## Examples

The repository includes several examples:

- [Chat track](https://github.com/moq-dev/moq/blob/main/rs/moq-native/examples/chat.rs) - Text messaging
- [Clock track](https://github.com/moq-dev/moq/tree/main/rs/moq-clock) - Timestamp synchronization

## API Reference

Full API documentation: [docs.rs/moq-lite](https://docs.rs/moq-lite)

Key types:

- `Connection` - Connection to a relay
- `BroadcastProducer` / `BroadcastConsumer` - Publish/subscribe to broadcasts
- `Track` - Named stream within a broadcast
- `Group` - Collection of frames
- `Frame` - Individual data chunk

## Protocol Specification

See the [moq-lite specification](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html) for protocol details.

## Next Steps

- Build media apps with [hang](/rust/hang)
- Deploy a [relay server](/rust/moq-relay)
- Read the [Architecture guide](/guide/architecture)
- View [code examples](/rust/examples)
