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

### Client & Server

See the [web-transport repository](https://github.com/moq-dev/web-transport) for client and server examples.

For a real-world example of using `web-transport` with MoQ, see the [`rs/moq-native/examples/chat.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-native/examples/chat.rs) example which demonstrates connection setup, publishing, and session management.

## Features

- **Streams** - Bidirectional and unidirectional streams
- **Datagrams** - Unreliable, unordered data
- **Session Management** - Peer/local addresses, graceful close
- **TLS** - Self-signed certificates (dev), Let's Encrypt (production), certificate fingerprints
- **WebSocket Polyfill** - See [web-transport-ws](https://github.com/moq-dev/web-transport/tree/main/web-transport-ws)

## Integration with MoQ

The `moq-lite` crate uses `web-transport` internally. See the [moq-native examples](https://github.com/moq-dev/moq/tree/main/rs/moq-native/examples) for how connections are established.

## Next Steps

- Check the [GitHub repository](https://github.com/moq-dev/web-transport)
- Use [moq-lite](/rs/crate/moq-lite) for MoQ protocol
- Deploy a [relay server](/app/relay/)
