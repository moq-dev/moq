---
title: Rust Libraries
description: Rust implementation of MoQ protocol and tools
---

# Rust Libraries

The Rust implementation provides the reference implementation of the MoQ protocol, along with server-side tools and native applications.

## Core Libraries

### moq-lite

[![crates.io](https://img.shields.io/crates/v/moq-lite)](https://crates.io/crates/moq-lite)
[![docs.rs](https://docs.rs/moq-lite/badge.svg)](https://docs.rs/moq-lite)

The core pub/sub transport protocol implementing the [moq-lite specification](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html).

**Features:**
- Broadcasts, tracks, groups, and frames
- Built-in concurrency and deduplication
- QUIC stream management
- Prioritization and backpressure

[Learn more →](/rust/moq-lite)

### hang

[![crates.io](https://img.shields.io/crates/v/hang)](https://crates.io/crates/hang)
[![docs.rs](https://docs.rs/hang/badge.svg)](https://docs.rs/hang)

Media-specific encoding/streaming library built on top of `moq-lite`.

**Features:**
- Catalog for track discovery
- Container format (timestamp + codec bitstream)
- Support for H.264/265, VP8/9, AV1, AAC, Opus
- CMAF/fMP4 import

[Learn more →](/rust/hang)

## Server Tools

### moq-relay

A stateless relay server that routes broadcasts between publishers and subscribers.

**Features:**
- Fan-out to multiple subscribers
- Cross-region clustering
- JWT-based authentication
- HTTP debugging endpoints

[Learn more →](/rust/moq-relay)

### moq-token

[![docs.rs](https://docs.rs/moq-token/badge.svg)](https://docs.rs/moq-token)

JWT authentication library and CLI tool for generating tokens.

**Features:**
- HMAC and RSA/ECDSA signing
- Path-based authorization
- Token generation and verification
- Available as library and CLI

See [Authentication guide](/guide/authentication)

## Utilities

### moq-native

[![docs.rs](https://docs.rs/moq-native/badge.svg)](https://docs.rs/moq-native)

Opinionated helpers to configure a Quinn QUIC endpoint.

**Features:**
- TLS certificate management
- QUIC transport configuration
- Connection setup helpers

### moq-clock

Timing and clock utilities for synchronization.

### libmoq

[![docs.rs](https://docs.rs/libmoq/badge.svg)](https://docs.rs/libmoq)

C bindings for `moq-lite` via FFI.

**Use cases:**
- Integrate with C/C++ applications
- Bindings for other languages
- Legacy system integration

## CLI Tools

### hang-cli

Command-line tool for media operations (binary name: `hang`).

**Features:**
- Publish video from files or FFmpeg
- Test and development
- Media server deployments

**Installation:**
```bash
cargo install hang-cli
```

**Usage:**
```bash
# Publish a video file
hang publish video.mp4

# Publish from FFmpeg
ffmpeg -i input.mp4 -f mpegts - | hang publish -
```

### moq-token-cli

Command-line tool for JWT token management (binary name: `moq-token`).

**Installation:**
```bash
cargo install moq-token-cli
```

**Usage:**
```bash
# Generate a key
moq-token --key root.jwk generate

# Sign a token
moq-token --key root.jwk sign \
  --root "rooms/123" \
  --publish "alice" \
  --expires 1735689600
```

See [Authentication guide](/guide/authentication) for details.

## Installation

### From crates.io

Add to your `Cargo.toml`:

```toml
[dependencies]
moq-lite = "0.1"
hang = "0.1"
```

### From Source

```bash
git clone https://github.com/moq-dev/moq
cd moq/rs
cargo build --release
```

### Using Nix

```bash
# Build moq-relay
nix build github:moq-dev/moq#moq-relay

# Build hang-cli
nix build github:moq-dev/moq#hang-cli
```

## Quick Start

### Publishing (Rust)

```rust
use moq_lite::*;
use tokio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to relay
    let connection = Connection::connect("https://relay.example.com/demo").await?;

    // Create a broadcast
    let mut broadcast = BroadcastProducer::new("my-broadcast");

    // Create a track
    let mut track = broadcast.create_track("chat");

    // Publish a group with a frame
    let mut group = track.append_group();
    group.write(b"Hello, MoQ!")?;
    group.close()?;

    // Publish to connection
    connection.publish(&mut broadcast).await?;

    Ok(())
}
```

### Subscribing (Rust)

```rust
use moq_lite::*;
use tokio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to relay
    let connection = Connection::connect("https://relay.example.com/demo").await?;

    // Subscribe to a broadcast
    let broadcast = connection.consume("my-broadcast").await?;

    // Subscribe to a track
    let mut track = broadcast.subscribe("chat").await?;

    // Read groups and frames
    while let Some(group) = track.next_group().await? {
        while let Some(frame) = group.read().await? {
            println!("Received: {:?}", frame);
        }
    }

    Ok(())
}
```

## API Documentation

Full API documentation is available on [docs.rs](https://docs.rs):

- [moq-lite API](https://docs.rs/moq-lite)
- [hang API](https://docs.rs/hang)
- [moq-token API](https://docs.rs/moq-token)
- [moq-native API](https://docs.rs/moq-native)
- [libmoq API](https://docs.rs/libmoq)

## Examples

The repository includes several examples:

- [Chat track](https://github.com/moq-dev/moq/blob/main/rs/moq-native/examples/chat.rs) - Publishing and subscribing to text
- [Clock track](https://github.com/moq-dev/moq/tree/main/rs/moq-clock) - Timestamp synchronization
- [Video track](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs) - Publishing video frames

[View more examples →](/rust/examples)

## Next Steps

- Explore [moq-lite](/rust/moq-lite) - Core protocol
- Explore [hang](/rust/hang) - Media library
- Deploy [moq-relay](/rust/moq-relay) - Relay server
- View [code examples](/rust/examples)
- Read [API reference](/api/rust)
