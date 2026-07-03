---
title: Rust Libraries
description: Rust implementation of MoQ protocol and tools
---

# Rust Libraries

The Rust implementation provides the reference implementation of the MoQ protocol, along with server-side tools and native applications.

## Core Libraries

### moq-net

[![crates.io](https://img.shields.io/crates/v/moq-net)](https://crates.io/crates/moq-net)
[![docs.rs](https://docs.rs/moq-net/badge.svg)](https://docs.rs/moq-net)

The networking layer for MoQ. At session setup it negotiates one of two wire protocols: the simplified [moq-lite](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/) protocol or the full IETF [moq-transport](https://datatracker.ietf.org/group/moq/documents/) protocol.

**Features:**

- Broadcasts, tracks, groups, and frames
- Built-in concurrency and deduplication
- QUIC stream management
- Prioritization and backpressure

[Learn more](/lib/rs/crate/moq-net)

### hang

[![crates.io](https://img.shields.io/crates/v/hang)](https://crates.io/crates/hang)
[![docs.rs](https://docs.rs/hang/badge.svg)](https://docs.rs/hang)

Media-specific encoding/streaming library built on top of `moq-net`.

**Features:**

- Catalog for track discovery
- Container format (timestamp + codec bitstream)
- Support for H.264/265, VP8/9, AV1, AAC, Opus

[Learn more](/lib/rs/crate/hang)

### moq-mux

[![crates.io](https://img.shields.io/crates/v/moq-mux)](https://crates.io/crates/moq-mux)
[![docs.rs](https://docs.rs/moq-mux/badge.svg)](https://docs.rs/moq-mux)

Media muxers and demuxers for importing existing formats into MoQ.

**Features:**

- fMP4/CMAF import
- MPEG-TS and FLV import/export
- H.264/H.265 Annex B parsing
- AAC and Opus codec support

[Learn more](/lib/rs/crate/moq-mux)

## Authentication

### moq-token

[![crates.io](https://img.shields.io/crates/v/moq-token)](https://crates.io/crates/moq-token)
[![docs.rs](https://docs.rs/moq-token/badge.svg)](https://docs.rs/moq-token)

JWT authentication library and CLI tool for generating tokens.

**Features:**

- HMAC and RSA/ECDSA signing
- Path-based authorization
- Token generation and verification
- Available as library and CLI

[Learn more](/lib/rs/crate/moq-token)

## Networking

### web-transport

QUIC and WebTransport implementation for Rust.

**Features:**

- Quinn-based QUIC
- WebTransport protocol support
- TLS certificate management
- Server and client modes

[Learn more](/lib/rs/crate/web-transport)

### moq-native

[![docs.rs](https://docs.rs/moq-native/badge.svg)](https://docs.rs/moq-native)

Opinionated helpers to configure a Quinn QUIC endpoint.

**Features:**

- TLS certificate management
- QUIC transport configuration
- Connection setup helpers

## CLI Tools

### moq-cli

Command-line tool for media operations (binary name: `moq-cli`).

**Features:**

- Publish video from files or FFmpeg
- Test and development
- Media server deployments

**Installation:**

```bash
cargo install moq-cli
```

**Usage:**

```bash
# Publish a video file (remux to MPEG-TS and pipe it in)
ffmpeg -i input.mp4 -c copy -f mpegts - | \
    moq --client-connect https://relay.example.com/anon --broadcast my-stream import ts

# Publish from FFmpeg
ffmpeg -i input.mp4 -f mpegts - | \
    moq --client-connect https://relay.example.com/anon --broadcast my-stream import ts
```

[Learn more](/bin/cli)

### moq-token-cli

Command-line tool for JWT token management (binary name: `moq-token-cli`).

**Installation:**

```bash
cargo install moq-token-cli
```

**Usage:**

```bash
# Generate a key
moq-token-cli generate --out root.jwk

# Sign a token
moq-token-cli sign --key root.jwk \
  --root "rooms/123" \
  --publish "alice" \
  --expires 1735689600
```

See [Authentication guide](/bin/relay/auth) for details.

## Utilities

### libmoq

[![docs.rs](https://docs.rs/libmoq/badge.svg)](https://docs.rs/libmoq)

C bindings for `moq-net` via FFI.

**Use cases:**

- Integrate with C/C++ applications
- Bindings for other languages
- Legacy system integration

## Installation

### Libraries (crates.io)

Add the crates you need to your `Cargo.toml`:

```toml
[dependencies]
moq-net = "0.1"
hang = "0.1"
```

All crates are published to [crates.io](https://crates.io/search?q=moq) with API
docs on [docs.rs](https://docs.rs).

### Binaries

The relay and CLI ship through several channels. Pick whichever fits:

```bash
# crates.io (any platform with a Rust toolchain)
cargo install moq-relay moq-cli moq-token-cli

# Homebrew (macOS / Linux)
brew install moq-dev/tap/moq-relay moq-dev/tap/moq-cli

# Debian / Ubuntu (see the Linux packages guide for repo setup)
sudo apt install moq-relay moq-cli

# Fedora / RHEL (see the Linux packages guide for repo setup)
sudo dnf install moq-relay moq-cli

# Nix
nix build github:moq-dev/moq#moq-relay
nix build github:moq-dev/moq#moq-cli

# Docker
docker run moqdev/moq-relay
docker run moqdev/moq-cli
```

See [Linux packages](/setup/linux) for apt/dnf repository setup and the
[Applications](/bin/) docs for usage.

### From source

```bash
git clone https://github.com/moq-dev/moq
cd moq/rs
cargo build --release
```

## Quick Start

[`moq-native`](/lib/rs/crate/moq-native) configures the QUIC endpoint and TLS for
you, then [`moq-net`](/lib/rs/crate/moq-net) handles the MoQ handshake. Connect to
a relay with a few lines:

```rust
let client = moq_native::ClientConfig::default().init()?;
let url = url::Url::parse("https://relay.moq.dev/anon")?;
let session = client.connect(url).await?;
```

To publish or consume, wire an [`Origin`](https://docs.rs/moq-net/latest/moq_net/struct.Origin.html)
into the session before connecting:

```rust
// Subscribe: wait for broadcasts to be announced.
let origin = moq_net::Origin::new().produce();
let mut consumer = origin.consume();
let session = client.with_consume(origin).connect(url).await?;

while let Some((path, broadcast)) = consumer.announced().await {
    // ... subscribe to tracks on each broadcast ...
}
```

The [Native guide](/lib/rs/env/native) walks through publishing, subscribing,
reading the catalog, and decoding frames end to end. For runnable code see the
[`hang/examples`](https://github.com/moq-dev/moq/tree/main/rs/hang/examples)
directory: [video.rs](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs)
(publish) and [subscribe.rs](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/subscribe.rs).

## API Documentation

Full API documentation is available on [docs.rs](https://docs.rs):

- [moq-net API](https://docs.rs/moq-net)
- [hang API](https://docs.rs/hang)
- [moq-mux API](https://docs.rs/moq-mux)
- [moq-token API](https://docs.rs/moq-token)
- [moq-native API](https://docs.rs/moq-native)
- [libmoq API](https://docs.rs/libmoq)

## Next Steps

- Explore [moq-net](/lib/rs/crate/moq-net) - Networking layer
- Explore [hang](/lib/rs/crate/hang) - Media library
- Explore [moq-mux](/lib/rs/crate/moq-mux) - Media import
- Deploy [moq-relay](/bin/relay/) - Relay server
- View [code examples](https://github.com/moq-dev/moq/tree/main/rs)
