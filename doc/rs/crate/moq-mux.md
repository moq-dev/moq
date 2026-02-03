---
title: moq-mux
description: Media muxers and demuxers for MoQ
---

# moq-mux

[![crates.io](https://img.shields.io/crates/v/moq-mux)](https://crates.io/crates/moq-mux)
[![docs.rs](https://docs.rs/moq-mux/badge.svg)](https://docs.rs/moq-mux)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

Media muxers and demuxers for converting existing media formats into MoQ broadcasts.

## Overview

`moq-mux` provides tools for importing media from various container formats:

- **fMP4/CMAF** - Fragmented MP4 and Common Media Application Format
- **HLS** - HTTP Live Streaming playlists
- **Annex B** - H.264/H.265 raw NAL unit streams

This crate is designed for ingesting existing content into the MoQ ecosystem, converting from traditional formats into [hang](/rs/crate/hang) broadcasts.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
moq-mux = "0.1"
```

### Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `mp4` | ✓ | fMP4/CMAF support |
| `h264` | ✓ | H.264 codec support |
| `h265` | ✓ | H.265 codec support |
| `hls` | ✓ | HLS playlist import |

## Quick Start

### Import fMP4 File

```rust
use moq_mux::import::*;
use hang::BroadcastProducer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a hang broadcast
    let broadcast = BroadcastProducer::new();

    // Create fMP4 importer
    let fmp4 = Fmp4::new(broadcast, Fmp4Config::default());

    // Import from file
    let file = tokio::fs::File::open("video.mp4").await?;
    let reader = tokio::io::BufReader::new(file);
    fmp4.decode(reader).await?;

    Ok(())
}
```

### Import HLS Stream

```rust
use moq_mux::import::*;
use hang::BroadcastProducer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a hang broadcast
    let broadcast = BroadcastProducer::new();

    // Create HLS importer with playlist URL
    let hls = Hls::new(
        broadcast,
        "https://example.com/stream.m3u8".parse()?,
    )?;

    // Run the importer (fetches and processes segments)
    hls.run().await?;

    Ok(())
}
```

## Supported Codecs

**Video:**
- H.264 (AVC) - requires `h264` feature
- H.265 (HEVC) - requires `h265` feature

**Audio:**
- AAC
- Opus

## Use Cases

- **Ingest existing content** - Convert VOD files to MoQ broadcasts
- **HLS bridge** - Re-publish HLS streams over MoQ for lower latency
- **Testing** - Use sample files for development and testing
- **Migration** - Transition from traditional streaming to MoQ

## Integration with hang

`moq-mux` produces [hang](/rs/crate/hang) broadcasts with proper catalog and frame metadata:

```rust
use moq_mux::import::*;
use hang::BroadcastProducer;
use moq_lite::Connection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to relay
    let connection = Connection::connect("https://relay.example.com/demo").await?;

    // Create broadcast
    let broadcast = BroadcastProducer::new();

    // Import media
    let fmp4 = Fmp4::new(broadcast.clone(), Fmp4Config::default());
    tokio::spawn(async move {
        let file = tokio::fs::File::open("video.mp4").await.unwrap();
        fmp4.decode(tokio::io::BufReader::new(file)).await.unwrap();
    });

    // Publish to relay
    connection.publish(broadcast).await?;

    Ok(())
}
```

## API Reference

Full API documentation: [docs.rs/moq-mux](https://docs.rs/moq-mux)

Key types:

- `Fmp4` - fMP4/CMAF importer
- `Fmp4Config` - Configuration for fMP4 import
- `Hls` - HLS playlist importer
- `Decoder` - Codec-specific decoders (AAC, Opus, AVC, HEVC)

## CLI Tool

For command-line importing, use [moq-cli](/app/cli):

```bash
# Install
cargo install moq-cli

# Publish a video file
moq publish video.mp4

# Publish from FFmpeg
ffmpeg -i input.mp4 -f mpegts - | moq publish -
```

## Next Steps

- Use [hang](/rs/crate/hang) for media encoding/decoding
- Use [moq-lite](/rs/crate/moq-lite) for the transport layer
- Deploy a [relay server](/app/relay/)
