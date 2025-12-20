---
title: Protocol
description: MoQ protocol specifications and standards
---

# Protocol Specifications

MoQ consists of two main protocol layers: `moq-lite` (transport) and `hang` (media).

## moq-lite

The core pub/sub transport protocol.

**Specification:** [draft-lcurley-moq-lite](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html)

### Overview

moq-lite provides a generic live data transport built on QUIC. It defines:

- **Broadcasts** - Collections of tracks
- **Tracks** - Named streams split into groups
- **Groups** - Sequential collections of frames
- **Frames** - Sized payloads

### Design Goals

1. **Generic** - Works for any live data, not just media
2. **Simple** - Easy to implement and deploy
3. **Efficient** - Leverages QUIC features for optimal delivery
4. **Scalable** - Designed for relay-based fan-out

### Key Features

- **Independent delivery** - Each group is a QUIC stream
- **Prioritization** - Send important groups first
- **Partial reliability** - Drop old groups when behind
- **Discovery** - Announce and subscribe to broadcasts
- **Deduplication** - Relay deduplicates shared subscriptions

### Message Types

The protocol defines several message types:

- `ANNOUNCE` - Publisher announces a broadcast
- `SUBSCRIBE` - Subscriber requests a track
- `SUBSCRIBE_OK` / `SUBSCRIBE_ERROR` - Subscription responses
- `UNSUBSCRIBE` - Cancel a subscription
- Stream headers - Metadata for groups and frames

See the [specification](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html) for complete protocol details.

## hang

Media-specific encoding/streaming protocol built on moq-lite.

**Specification:** [draft-lcurley-moq-hang](https://moq-dev.github.io/drafts/draft-lcurley-moq-hang.html)

### Overview

hang provides a simple media layer optimized for WebCodecs and modern browsers. It defines:

- **Catalog** - JSON track describing available tracks
- **Container** - Frame format: timestamp + codec bitstream

### Design Goals

1. **WebCodecs compatible** - Works with browser APIs
2. **Simple container** - Minimal overhead
3. **Codec agnostic** - Supports any codec
4. **Quality selection** - Enable adaptive bitrate

### Catalog Format

The catalog is a special track (usually named `catalog`) containing JSON:

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

This enables:
- Dynamic track discovery
- Codec negotiation
- Quality/bitrate selection
- Alternative tracks (multi-bitrate, multi-language)

### Frame Container

Each frame consists of:

```
┌─────────────────┐
│ Timestamp (u64) │  Presentation time in microseconds
├─────────────────┤
│ Codec Bitstream │  Raw encoded data
└─────────────────┘
```

This simple format:
- Works directly with WebCodecs
- Minimal parsing overhead
- Codec-agnostic
- Supports any timestamp base

### Supported Codecs

**Video:**
- H.264 (AVC)
- H.265 (HEVC)
- VP8
- VP9
- AV1

**Audio:**
- Opus
- AAC
- MP3

The codec string follows [RFC 6381](https://tools.ietf.org/html/rfc6381) format for compatibility with WebCodecs.

## Use Cases

**Specification:** [draft-lcurley-moq-use-cases](https://moq-dev.github.io/drafts/draft-lcurley-moq-use-cases.html)

This document describes various use cases and how to implement them with MoQ:

- Live video streaming
- Audio conferencing
- Screen sharing
- Text chat
- Gaming
- IoT/telemetry
- Collaborative editing

## Relationship to IETF MoQ

This project is a [fork](https://moq.dev/blog/transfork) of the [IETF MoQ Working Group](https://datatracker.ietf.org/group/moq/documents/) specification.

### Key Differences

**Scope:**
- IETF MoQ: Broad, general-purpose media transport
- This project: Narrower focus on deployability and simplicity

**Design:**
- IETF MoQ: Feature-rich with many optional extensions
- This project: Minimal, opinionated design

**Status:**
- IETF MoQ: Ongoing standardization process
- This project: Production-ready implementation

### Why Fork?

The fork prioritizes:
1. **Simplicity** - Fewer concepts, easier to implement
2. **Deployability** - Can deploy today with existing infrastructure
3. **Focus** - Optimized for live streaming use cases

Both efforts share knowledge and collaborate where beneficial.

## Protocol Implementation

### Rust

The Rust implementation is the reference:

- **moq-lite** - [crates.io](https://crates.io/crates/moq-lite) | [docs.rs](https://docs.rs/moq-lite)
- **hang** - [crates.io](https://crates.io/crates/hang) | [docs.rs](https://docs.rs/hang)

See [Rust libraries](/rust/) for details.

### TypeScript

The TypeScript implementation for browsers:

- **@moq/lite** - [npm](https://www.npmjs.com/package/@moq/lite)
- **@moq/hang** - [npm](https://www.npmjs.com/package/@moq/hang)

See [TypeScript libraries](/typescript/) for details.

### C Bindings

C bindings via FFI:

- **libmoq** - [docs.rs](https://docs.rs/libmoq)

## Protocol Evolution

The protocol is actively developed but aims for stability:

- **moq-lite** - Core is stable, extensions possible
- **hang** - Open to codec and container improvements

Changes follow semantic versioning with backwards compatibility where possible.

## Contributing to Specifications

Specifications are maintained in the [moq-dev/drafts](https://github.com/moq-dev/drafts) repository.

Contributions welcome:
- Issue feedback and suggestions
- Propose clarifications
- Submit use cases
- Report implementation issues

## Next Steps

- Understand the [Architecture](/guide/architecture)
- Learn about [Authentication](/guide/authentication)
- Try the [Rust libraries](/rust/) or [TypeScript libraries](/typescript/)
- Deploy with the [Deployment guide](/guide/deployment)
