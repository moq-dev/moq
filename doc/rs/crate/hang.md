---
title: hang
description: Media library built on moq-lite
---

# hang

[![crates.io](https://img.shields.io/crates/v/hang)](https://crates.io/crates/hang)
[![docs.rs](https://docs.rs/hang/badge.svg)](https://docs.rs/hang)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

A media library built on top of [moq-lite](/rs/crate/moq-lite) for streaming audio and video.

## Overview

`hang` provides media-specific functionality on top of the generic `moq-lite` transport:

- **Broadcast** - Discoverable collection of tracks with catalog
- **Catalog** - Metadata describing available tracks, codec info, etc. (updated live)
- **Track** - Audio/video streams and other data types
- **Group** - Group of pictures (video) or collection of samples (audio)
- **Frame** - Timestamp + codec payload pair

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
hang = "0.1"
```

## Supported Codecs

`hang` implements most of the [WebCodecs specification](https://www.w3.org/TR/webcodecs/).

**Video:**
- H.264 (AVC)
- H.265 (HEVC)
- VP8
- VP9
- AV1

**Audio:**
- AAC
- Opus

## Quick Start

### Publishing Video

```rust
use hang::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to relay
    let connection = moq_lite::Connection::connect(
        "https://relay.example.com/demo"
    ).await?;

    // Create a hang broadcast
    let mut broadcast = Broadcast::new("my-stream");

    // Create video track
    let video_track = broadcast.create_video_track(VideoConfig {
        codec: "avc1.64002a".to_string(),
        width: 1920,
        height: 1080,
        framerate: 30.0,
        bitrate: 5_000_000,
    })?;

    // Create audio track
    let audio_track = broadcast.create_audio_track(AudioConfig {
        codec: "opus".to_string(),
        sample_rate: 48000,
        channels: 2,
        bitrate: 128_000,
    })?;

    // Publish encoded frames
    video_track.append_frame(Frame {
        timestamp: 0,
        data: h264_keyframe_data,
        is_keyframe: true,
    })?;

    audio_track.append_frame(Frame {
        timestamp: 0,
        data: opus_packet_data,
        is_keyframe: false,
    })?;

    // Publish to relay
    connection.publish_broadcast(broadcast).await?;

    Ok(())
}
```

### Subscribing to Video

```rust
use hang::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to relay
    let connection = moq_lite::Connection::connect(
        "https://relay.example.com/demo"
    ).await?;

    // Subscribe to broadcast
    let broadcast = connection.consume_broadcast("my-stream").await?;

    // Read catalog to discover tracks
    let catalog = broadcast.catalog().await?;

    for track_info in catalog.tracks {
        println!("Track: {} ({})", track_info.name, track_info.codec);

        if track_info.kind == "video" {
            // Subscribe to video track
            let track = broadcast.subscribe(&track_info.name).await?;

            // Read frames
            while let Some(frame) = track.next_frame().await? {
                println!("Video frame: {}µs, {} bytes",
                    frame.timestamp, frame.data.len());
                // Decode with your video decoder
            }
        }
    }

    Ok(())
}
```

## Catalog

The catalog is a special track containing JSON metadata about available tracks:

```json
{
  "version": 1,
  "tracks": [
    {
      "name": "video",
      "kind": "video",
      "codec": "avc1.64002a",
      "width": 1920,
      "height": 1080,
      "framerate": 30,
      "bitrate": 5000000
    },
    {
      "name": "audio",
      "kind": "audio",
      "codec": "opus",
      "sampleRate": 48000,
      "channelConfig": "2",
      "bitrate": 128000
    }
  ]
}
```

The catalog is updated live as tracks are added, removed, or changed.

## Frame Container

Each frame in `hang` consists of:

```rust
pub struct Frame {
    pub timestamp: u64,      // Microseconds
    pub data: Vec<u8>,       // Codec bitstream
    pub is_keyframe: bool,   // Keyframe flag
}
```

This simple container:
- Works with WebCodecs
- Minimal overhead
- Codec-agnostic
- Any timestamp base

## CMAF Import

`hang` includes a `cmaf` module to import fMP4/CMAF files:

```rust
use hang::cmaf::*;

// Import CMAF file
let cmaf_data = std::fs::read("video.mp4")?;
let broadcast = Broadcast::from_cmaf(&cmaf_data)?;

// Publish to relay
connection.publish_broadcast(broadcast).await?;
```

This is useful for:
- Ingesting existing content
- Converting VOD to live
- Testing with sample files

Note: The CMAF importer is basic and doesn't support all features.

## Grouping

Groups are aligned with natural boundaries:

**Video:**
- Start with keyframe (I-frame)
- Include dependent frames (P/B-frames)
- Enable joining at group boundaries

**Audio:**
- Collection of audio packets
- Usually 1 second of audio
- Independent decoding

```rust
let mut group = track.new_group();
group.append(keyframe)?;
group.append(p_frame_1)?;
group.append(p_frame_2)?;
group.finalize()?;
```

## Prioritization

`hang` automatically prioritizes:

1. **Keyframes** - Highest priority (can't decode without them)
2. **Recent frames** - Higher priority than old frames
3. **Audio** - Often prioritized over video

This is handled automatically based on frame metadata.

## CLI Tool

The `hang-cli` package provides a command-line tool (binary name: `hang`):

```bash
# Install
cargo install hang-cli

# Publish a video file
hang publish video.mp4

# Publish from FFmpeg
ffmpeg -i input.mp4 -f mpegts - | hang publish -

# Custom encoding settings
hang publish --codec h264 --bitrate 2000000 video.mp4
```

See `hang --help` for all options, or [FFmpeg documentation](/app/cli).

## API Reference

Full API documentation: [docs.rs/hang](https://docs.rs/hang)

Key types:

- `Broadcast` - Media broadcast with catalog
- `Catalog` - Track metadata
- `VideoConfig` / `AudioConfig` - Track configuration
- `Frame` - Timestamp + codec bitstream
- `cmaf` module - CMAF/fMP4 import

## Protocol Specification

See the [hang specification](https://moq-dev.github.io/drafts/draft-lcurley-moq-hang.html) for protocol details.

## Next Steps

- Use the [moq-lite](/rs/crate/moq-lite) transport layer
- Deploy a [relay server](/app/relay/)
- Read the [Concepts guide](/concept/)
- View [code examples](https://github.com/moq-dev/moq/tree/main/rs)
