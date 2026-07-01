---
title: hang
description: Media library built on moq-net
---

# hang

[![crates.io](https://img.shields.io/crates/v/hang)](https://crates.io/crates/hang)
[![docs.rs](https://docs.rs/hang/badge.svg)](https://docs.rs/hang)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

A media library built on top of [moq-net](/lib/rs/crate/moq-net) for streaming audio and video.

## Overview

`hang` provides media-specific functionality on top of the generic `moq-net` transport:

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

See [`rs/hang/examples/video.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs) for a complete example of creating a broadcast with a video track, catalog, and publishing frames.

### Subscribing to Video

See [`rs/hang/examples/subscribe.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/subscribe.rs) for a complete example of subscribing to a broadcast, reading the catalog, and consuming video frames.

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

Each frame in `hang` consists of a timestamp and codec bitstream payload. See the [video example](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs) for the `Frame` struct in action.

## CMAF Import

For importing fMP4/CMAF files, see the [moq-mux](/lib/rs/crate/moq-mux) crate. For HLS, see [moq-hls](https://github.com/moq-dev/moq/tree/main/rs/moq-hls).

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

See the [video example](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs) for grouping with `OrderedProducer`.

## Prioritization

`hang` automatically prioritizes:

1. **Keyframes** - Highest priority (can't decode without them)
2. **Recent frames** - Higher priority than old frames
3. **Audio** - Often prioritized over video

This is handled automatically based on frame metadata.

## CLI Tool

The `moq-cli` package provides a command-line tool (binary name: `moq-cli`):

```bash
# Install
cargo install moq-cli

# Publish a video file (remux to MPEG-TS and pipe it in)
ffmpeg -i input.mp4 -c copy -f mpegts - | \
    moq --client-connect https://relay.example.com/anon --broadcast my-stream import ts

# Publish from FFmpeg
ffmpeg -i input.mp4 -f mpegts - | \
    moq --client-connect https://relay.example.com/anon --broadcast my-stream import ts
```

See `moq --help` for all options, or [FFmpeg documentation](/bin/cli).

## API Reference

Full API documentation: [docs.rs/hang](https://docs.rs/hang)

Key types:

- `Broadcast` - Media broadcast with catalog
- `Catalog` - Track metadata
- `VideoConfig` / `AudioConfig` - Track configuration
- `Frame` - Timestamp + codec bitstream
- [moq-mux](/lib/rs/crate/moq-mux) - CMAF/fMP4 import

## Protocol Specification

See the [hang specification](https://datatracker.ietf.org/doc/draft-lcurley-moq-hang/) for protocol details.

## Next Steps

- Use the [moq-net](/lib/rs/crate/moq-net) transport layer
- Deploy a [relay server](/bin/relay/)
- Read the [Concepts guide](/concept/)
- View [code examples](https://github.com/moq-dev/moq/tree/main/rs)
