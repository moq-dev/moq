---
title: web-transport
description: QUIC and WebTransport implementation for Rust
---

# web-transport

QUIC and WebTransport implementation for Rust, providing the networking layer for MoQ.

::: info External Repository
This crate is maintained in a separate repository: [moq-dev/web-transport](https://github.com/moq-dev/web-transport)
:::

## Overview

The `web-transport` crate provides:

- **QUIC client and server** - Built on Quinn
- **WebTransport protocol** - HTTP/3 based transport
- **Browser compatibility** - Same protocol as browser WebTransport API
- **TLS management** - Certificate handling utilities

## Repository

**GitHub:** [moq-dev/web-transport](https://github.com/moq-dev/web-transport)

## Crates

The repository contains multiple crates:

| Crate | Description |
|-------|-------------|
| `web-transport` | Core WebTransport implementation |
| `web-transport-quinn` | Quinn-based QUIC transport |
| `web-transport-ws` | WebSocket polyfill for non-WebTransport browsers |

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
web-transport = "0.1"
web-transport-quinn = "0.1"
```

## Quick Start

### Client

```rust
use web_transport_quinn::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create client
    let client = Client::new()?;

    // Connect to server
    let session = client.connect("https://server.example.com").await?;

    // Open a bidirectional stream
    let (send, recv) = session.open_bi().await?;

    // Send data
    send.write_all(b"Hello, WebTransport!").await?;

    // Receive data
    let mut buf = vec![0u8; 1024];
    let n = recv.read(&mut buf).await?;

    Ok(())
}
```

### Server

```rust
use web_transport_quinn::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load TLS certificate
    let cert = Certificate::load("cert.pem", "key.pem")?;

    // Create server
    let server = Server::new(cert)?;

    // Bind to address
    let endpoint = server.bind("[::]:4443").await?;

    // Accept connections
    while let Some(session) = endpoint.accept().await {
        tokio::spawn(async move {
            handle_session(session).await;
        });
    }

    Ok(())
}

async fn handle_session(session: Session) {
    // Accept bidirectional streams
    while let Some((send, recv)) = session.accept_bi().await {
        // Handle stream...
    }
}
```

## Features

### Streams

WebTransport supports multiple stream types:

```rust
// Bidirectional stream
let (send, recv) = session.open_bi().await?;

// Unidirectional stream (send only)
let send = session.open_uni().await?;

// Accept incoming unidirectional stream
let recv = session.accept_uni().await?;
```

### Datagrams

For unreliable, unordered data:

```rust
// Send datagram
session.send_datagram(data).await?;

// Receive datagram
let data = session.receive_datagram().await?;
```

### Session Management

```rust
// Get session info
let peer_addr = session.peer_addr();
let local_addr = session.local_addr();

// Close session
session.close(0, b"goodbye").await?;
```

## TLS Configuration

### Self-Signed Certificates

For development:

```rust
let cert = Certificate::generate_self_signed("localhost")?;
```

### Let's Encrypt Certificates

For production:

```rust
let cert = Certificate::load(
    "/etc/letsencrypt/live/example.com/fullchain.pem",
    "/etc/letsencrypt/live/example.com/privkey.pem"
)?;
```

### Certificate Fingerprints

For browser connections with self-signed certs:

```rust
let fingerprint = cert.fingerprint_sha256();
// Pass to browser: serverCertificateHashes option
```

## WebSocket Polyfill

For browsers without WebTransport support:

```rust
use web_transport_ws::*;

// Server accepts both WebTransport and WebSocket
let server = HybridServer::new(cert)?;
```

See [web-transport-ws](https://github.com/moq-dev/web-transport/tree/main/web-transport-ws) for details.

## Integration with MoQ

The `moq-lite` crate uses `web-transport` internally:

```rust
use moq_lite::*;

// Connection uses WebTransport under the hood
let connection = Connection::connect("https://relay.example.com/demo").await?;
```

For custom transport configuration, use the lower-level API:

```rust
use moq_lite::*;
use web_transport_quinn::*;

let client = Client::with_config(custom_config)?;
let session = client.connect("https://relay.example.com").await?;
let connection = Connection::from_session(session);
```

## Next Steps

- Check the [GitHub repository](https://github.com/moq-dev/web-transport)
- Use [moq-lite](/rs/crate/moq-lite) for MoQ protocol
- Deploy a [relay server](/app/relay/)
