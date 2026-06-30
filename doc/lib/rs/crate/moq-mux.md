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

`moq-mux` provides tools for importing media from (and exporting it back to) various container formats:

- **fMP4/CMAF** - Fragmented MP4 and Common Media Application Format
- **MPEG-TS** - Transport stream (import and export)
- **Matroska / WebM** - EBML container (import and export)
- **FLV** - Flash Video / RTMP container (H.264 + AAC; import and export)
- **Annex B** - H.264/H.265 raw NAL unit streams

This crate is designed for ingesting existing content into the MoQ ecosystem, converting from traditional formats into [hang](/lib/rs/crate/hang) broadcasts.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
moq-mux = "0.1"
```

## Quick Start

### Import fMP4

See the [moq-cli source](https://github.com/moq-dev/moq/tree/main/rs/moq-cli) for real-world usage of `moq-mux` for importing fMP4 streams. Use [moq-hls](https://github.com/moq-dev/moq/tree/main/rs/moq-hls) for HLS import and export.

## Supported Codecs

**Video:**

- H.264 (AVC) - requires `h264` feature
- H.265 (HEVC) - requires `h265` feature
- AV1
- VP8
- VP9

**Audio:**

- AAC
- Opus
- MP2 (MPEG-TS only, carried verbatim)
- AC-3 (MPEG-TS only, carried verbatim)
- E-AC-3 (MPEG-TS only, carried verbatim)

## Use Cases

- **Ingest existing content** - Convert VOD files to MoQ broadcasts
- **Testing** - Use sample files for development and testing
- **Migration** - Transition from traditional streaming to MoQ

## Integration with hang

`moq-mux` produces [hang](/lib/rs/crate/hang) broadcasts with proper catalog and frame metadata. See the [hang video example](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs) for how to publish a broadcast with proper catalog setup.

## API Reference

Full API documentation: [docs.rs/moq-mux](https://docs.rs/moq-mux)

Key types:

- `Fmp4` - fMP4/CMAF importer
- `Fmp4Config` - Configuration for fMP4 import
- `Decoder` - Codec-specific decoders (AAC, Opus, AVC, HEVC)

## CLI Tool

For command-line importing, use [moq-cli](/bin/cli):

```bash
# Install
cargo install moq-cli

# Publish a video file (remux to MPEG-TS and pipe it in)
ffmpeg -i input.mp4 -c copy -f mpegts - | \
    moq-cli publish --url https://relay.example.com/anon --broadcast my-stream ts

# Publish from FFmpeg
ffmpeg -i input.mp4 -f mpegts - | \
    moq-cli publish --url https://relay.example.com/anon --broadcast my-stream ts
```

## Next Steps

- Use [hang](/lib/rs/crate/hang) for media encoding/decoding
- Use [moq-net](/lib/rs/crate/moq-net) for the transport layer
- Deploy a [relay server](/bin/relay/)
