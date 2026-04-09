---
title: moq-lite API Reference
description: Complete API reference with usage examples for moq-lite Rust crate
---

# moq-lite API Reference

This guide provides detailed API documentation with concrete usage examples for the `moq-lite` Rust crate.

## Table of Contents

1. [Quick Start](#quick-start)
2. [Core Types](#core-types)
3. [Client API](#client-api)
4. [Server API](#server-api)
5. [Session Management](#session-management)
6. [Common Patterns](#common-patterns)

---

## Quick Start

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
moq-lite = "0.1"
```

### Basic Client Example

```rust
use moq_lite::{Client, Session, Error};

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Create a client with both publish and consume capabilities
    let client = Client::new()
        .with_publish(true)  // Enable publishing
        .with_consume(true); // Enable subscribing
    
    // Connect to a relay server (requires WebTransport session)
    // let session = web_transport_client.connect("https://relay.example.com").await?;
    // let moq_session = client.connect(session).await?;
    
    Ok(())
}
```

---

## Core Types

### Hierarchy

```
Origin (Producer/Consumer)
  └── Broadcast (collection of Tracks)
        └── Track (collection of Groups)
              └── Group (collection of Frames)
                    └── Frame (data chunk)
```

### OriginProducer

Creates and manages broadcasts for publishing.

```rust
use moq_lite::OriginProducer;

// Create a new origin producer
let origin = OriginProducer::new();

// Create a new broadcast
let broadcast = origin.create_broadcast("live/camera1");

// Publish tracks within the broadcast
let track = broadcast.create_track("video");
```

**Key Methods:**
- `new()` - Create a new origin producer
- `create_broadcast(path: &str)` - Create a broadcast with given path
- `consume()` - Get paired `OriginConsumer` for the same origin

### OriginConsumer

Subscribes to broadcasts from an origin.

```rust
use moq_lite::OriginConsumer;

let consumer = OriginConsumer::new();

// Subscribe to a broadcast
let broadcast = consumer.subscribe_broadcast("live/camera1").await?;

// Get tracks from the broadcast
let track = broadcast.get_track("video").await?;
```

### Session

Represents a MoQ transport session over WebTransport.

```rust
use moq_lite::Session;

// Session is created via Client::connect or Server::accept
// let session = client.connect(webtransport_session).await?;

// Get protocol version
let version = session.version();

// Get bandwidth estimates
if let Some(bw) = session.send_bandwidth() {
    println!("Estimated send bitrate: {:?}", bw);
}

// Close session
session.close(Error::Cancel);
```

**Key Methods:**
- `version()` - Returns negotiated protocol version
- `send_bandwidth()` - Get send bandwidth consumer (if supported)
- `recv_bandwidth()` - Get receive bandwidth consumer (moq-lite-03+)
- `close(err)` - Close the session
- `closed()` - Wait for session to close

---

## Client API

### Client Builder

```rust
use moq_lite::Client;

// Basic client (no publish/consume)
let client = Client::new();

// Client with publish only
let client = Client::new()
    .with_publish(true);

// Client with consume only
let client = Client::new()
    .with_consume(true);

// Client with both
let origin = OriginProducer::new();
let client = Client::new()
    .with_origin(origin); // Sets both publish and consume
```

### with_publish()

Enable publishing capability.

```rust
let client = Client::new()
    .with_publish(origin_consumer); // Pass OriginConsumer for publishing
```

**Parameters:**
- `publish: impl Into<Option<OriginConsumer>>` - Consumer for publishing

### with_consume()

Enable subscription capability.

```rust
let client = Client::new()
    .with_consume(origin_producer); // Pass OriginProducer for consuming
```

**Parameters:**
- `consume: impl Into<Option<OriginProducer>>` - Producer for consuming

### with_versions()

Set supported protocol versions.

```rust
use moq_lite::{Client, Versions, Version};

let client = Client::new()
    .with_versions(Versions::all()); // Support all versions

// Or specify specific versions
let client = Client::new()
    .with_versions(Versions::new()
        .add(Version::Lite(lite::Version::Lite04)));
```

### connect()

Establish MoQ session over WebTransport.

```rust
use moq_lite::Client;

let client = Client::new()
    .with_publish(true)
    .with_consume(true);

// Requires a WebTransport session
// let moq_session = client.connect(webtransport_session).await?;
```

**Returns:**
- `Result<Session, Error>` - MoQ session on success

**Errors:**
- `Error::Version` - No compatible version found
- `Error::UnknownAlpn` - Unknown ALPN protocol
- `Error::Transport` - WebTransport connection failed

---

## Server API

### Server Builder

```rust
use moq_lite::Server;

let server = Server::new()
    .with_publish(true)
    .with_consume(true);
```

### accept()

Accept incoming client connection.

```rust
use moq_lite::Server;

let server = Server::new();

// Accept a client (requires WebTransport server)
// let session = server.accept(webtransport_session).await?;
```

---

## Session Management

### Bandwidth Estimation

```rust
use moq_lite::Session;

// Get send bandwidth (from QUIC congestion controller)
if let Some(bw) = session.send_bandwidth() {
    let rate = bw.get();
    println!("Send bitrate: {:?}", rate);
}

// Get receive bandwidth (moq-lite-03+ with PROBE support)
if let Some(bw) = session.recv_bandwidth() {
    let rate = bw.get();
    println!("Receive bitrate: {:?}", rate);
}
```

### Session Lifecycle

```rust
use moq_lite::{Session, Error};

// Session is cloneable, share across tasks
let session_clone = session.clone();

// Wait for session to close
tokio::spawn(async move {
    if let Err(e) = session_clone.closed().await {
        eprintln!("Session closed: {}", e);
    }
});

// Graceful shutdown
session.close(Error::Cancel);
```

---

## Common Patterns

### Pattern 1: Simple Publisher

```rust
use moq_lite::{Client, OriginProducer};

async fn publish_stream() -> Result<(), moq_lite::Error> {
    let origin = OriginProducer::new();
    let client = Client::new().with_origin(origin.clone());
    
    // Connect (requires WebTransport setup)
    // let session = client.connect(webtransport_session).await?;
    
    // Create broadcast
    let broadcast = origin.create_broadcast("live/camera1");
    
    // Create video track
    let video_track = broadcast.create_track("video");
    
    // Publish frames...
    // video_track.append_group(...).await?;
    
    Ok(())
}
```

### Pattern 2: Simple Subscriber

```rust
use moq_lite::{Client, OriginConsumer};

async fn subscribe_stream() -> Result<(), moq_lite::Error> {
    let origin = OriginConsumer::new();
    let client = Client::new().with_consume(origin.clone());
    
    // Connect
    // let session = client.connect(webtransport_session).await?;
    
    // Subscribe to broadcast
    let broadcast = origin.subscribe_broadcast("live/camera1").await?;
    
    // Get video track
    let video_track = broadcast.get_track("video").await?;
    
    // Receive frames...
    // while let Some(group) = video_track.next_group().await? { ... }
    
    Ok(())
}
```

### Pattern 3: Multi-Track Publisher

```rust
use moq_lite::{Client, OriginProducer};

async fn publish_multi_track() -> Result<(), moq_lite::Error> {
    let origin = OriginProducer::new();
    let client = Client::new().with_origin(origin.clone());
    
    // let session = client.connect(webtransport_session).await?;
    
    let broadcast = origin.create_broadcast("live/stream1");
    
    // Video track (high priority)
    let video = broadcast.create_track("video");
    
    // Audio track
    let audio = broadcast.create_track("audio");
    
    // Metadata track
    let metadata = broadcast.create_track("metadata");
    
    // Publish to each track independently
    // video.append_group(...).await?;
    // audio.append_group(...).await?;
    
    Ok(())
}
```

### Pattern 4: Error Handling

```rust
use moq_lite::{Client, Error, Session};

async fn connect_with_retry() -> Result<Session, Error> {
    let client = Client::new().with_publish(true).with_consume(true);
    
    let mut attempts = 0;
    loop {
        match client.connect(webtransport_session.clone()).await {
            Ok(session) => return Ok(session),
            Err(Error::Transport(e)) if attempts < 3 => {
                attempts += 1;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e),
        }
    }
}
```

---

## Error Types

### Error Enum

```rust
pub enum Error {
    Version,           // No compatible protocol version
    UnknownAlpn(String), // Unknown ALPN protocol
    Transport(String),  // WebTransport error
    Cancel,            // Session cancelled
    // ... other variants
}
```

### Error Handling

```rust
use moq_lite::Error;

fn handle_error(err: Error) {
    match err {
        Error::Version => eprintln!("Protocol version mismatch"),
        Error::UnknownAlpn(alpn) => eprintln!("Unknown ALPN: {}", alpn),
        Error::Transport(e) => eprintln!("Transport error: {}", e),
        Error::Cancel => eprintln!("Session cancelled"),
        _ => eprintln!("Error: {:?}", err),
    }
}
```

---

## Protocol Versions

### Supported Versions

| Version | ALPN | Description |
|---------|------|-------------|
| Draft-17 | `moq-17` | Latest IETF draft |
| Draft-16 | `moq-16` | IETF draft-16 |
| Draft-15 | `moq-15` | IETF draft-15 |
| Draft-14 | `moq-14` | IETF draft-14 |
| Lite-04 | `moq-lite-04` | moq-lite draft-04 |
| Lite-03 | `moq-lite-03` | moq-lite draft-03 (PROBE support) |

### Version Selection

```rust
use moq_lite::{Client, Versions, Version, lite};

// Support all versions
let client = Client::new()
    .with_versions(Versions::all());

// Support only moq-lite-04
let client = Client::new()
    .with_versions(Versions::new()
        .add(Version::Lite(lite::Version::Lite04)));
```

---

## See Also

- [Concepts Guide](/concept/) - Architecture overview
- [Setup Guide](/setup/) - Getting started
- [Demo](https://moq.dev/) - Live examples
- [docs.rs/moq-lite](https://docs.rs/moq-lite) - Rust API docs
- [Source Code](https://github.com/moq-dev/moq/tree/main/rs/moq-lite)

---

*Last updated: 2026-04-10*
*Contributed by @murraywu*
