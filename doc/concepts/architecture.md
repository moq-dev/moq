---
title: Architecture
description: Understanding MoQ's layered architecture
---

# Architecture

MoQ is designed as a layered protocol stack where each layer has distinct responsibilities. This separation enables flexibility, simplicity, and powerful use cases.

## The Golden Rule

**The CDN MUST NOT know anything about your application, media codecs, or even the available tracks.**

Everything could be fully end-to-end encrypted and the CDN wouldn't care. No business logic allowed.

Instead, `moq-relay` operates on rules encoded in the `moq-lite` header. These rules are based on video encoding patterns but are generic enough to be used for any live data.

## Protocol Layers

```
┌─────────────────┐
│   Application   │   🏢 Your business logic
│                 │    - authentication, non-media tracks, etc.
├─────────────────┤
│      hang       │   🎬 Media-specific encoding/streaming
│                 │     - codecs, containers, catalog
├─────────────────├
│    moq-lite     │  🚌 Generic pub/sub transport
│                 │     - broadcasts, tracks, groups, frames
├─────────────────┤
│  WebTransport   │  🌐 Browser-compatible QUIC
│      QUIC       │     - HTTP/3 handshake, multiplexing, etc.
└─────────────────┘
```

## Layer Details

### QUIC Layer

The foundation providing:

- **Streams** - Independent, ordered, reliable delivery channels
- **Multiplexing** - Many streams over a single connection
- **Prioritization** - Send important data first
- **Partial reliability** - Reset streams carrying stale data
- **Security** - TLS 1.3 built-in
- **0-RTT** - Resume connections without handshake

MoQ maps each group to a QUIC stream for independent delivery and prioritization.

### WebTransport Layer

A browser API exposing QUIC to web applications:

- **HTTP/3 based** - Leverages existing web infrastructure
- **Bidirectional streams** - For request/response patterns
- **Unidirectional streams** - For efficient one-way data
- **Datagrams** - For unreliable, low-latency messages

In browsers, WebTransport is the only way to access QUIC. On the server side, native QUIC libraries like [Quinn](https://github.com/quinn-rs/quinn) provide more control.

### moq-lite Layer

The core pub/sub transport protocol:

**Primitives:**
- **Broadcasts** - Collections of related tracks (like a "room" or "channel")
- **Tracks** - Named streams of sequential groups
- **Groups** - Collections of frames with independent delivery
- **Frames** - Sized payloads of bytes

**Features:**
- Built-in concurrency and deduplication
- Group-level prioritization rules
- Track discovery and announcement
- Path-based scoping

**Key insight:** The relay operates purely on these primitives without understanding what's inside frames. This enables:
- End-to-end encryption
- Custom media formats
- Non-media use cases
- Simple relay implementation

Think of `moq-lite` as **HTTP** - a generic transport layer.

### hang Layer

Media-specific encoding/streaming built on top of `moq-lite`:

**Components:**
- **Catalog** - JSON track listing available tracks and their properties
- **Container** - Simple frame format: timestamp + codec bitstream
- **Codecs** - Support for H.264/265, VP8/9, AV1, AAC, Opus

**Features:**
- WebCodecs compatibility
- Dynamic quality selection
- Track discovery via catalog
- Media-agnostic relay

Think of `hang` as **HLS/DASH** - a media-specific format.

The beauty: if you want something custom, extend or replace `hang` while keeping `moq-lite` unchanged.

### Application Layer

Your business logic:

- Authentication and authorization
- Custom track types (chat, telemetry, etc.)
- Application-specific metadata
- User interface and experience

## Data Flow

### Publishing Flow

1. **Capture** - Get media from camera, file, or generator
2. **Encode** - Convert to codec bitstream (H.264, Opus, etc.)
3. **Group** - Organize frames into groups (typically keyframe + deps)
4. **Wrap** - Add timestamp and metadata
5. **Publish** - Send to relay via `moq-lite`

### Relay Flow

1. **Accept** - Publisher connects and authenticates
2. **Store** - Keep recent groups in memory (cache)
3. **Route** - Forward to subscribers based on path
4. **Prioritize** - Send keyframes and recent data first
5. **Backpressure** - Drop old data when subscriber is slow

### Subscription Flow

1. **Connect** - Subscriber connects to relay
2. **Subscribe** - Request specific tracks
3. **Receive** - Get groups as QUIC streams
4. **Decode** - Extract frames and decode codec bitstream
5. **Render** - Display video/audio or process data

## Component Architecture

### moq-relay

A stateless relay server that:

- Accepts WebTransport connections
- Routes broadcasts between publishers and subscribers
- Performs fan-out to multiple subscribers
- Supports clustering for geographic distribution
- Uses JWT tokens for authentication
- Runs on any cloud provider with UDP support

Relay instances can be clustered:
- Each relay connects to others
- Publishers can be in different regions
- Subscribers connect to nearest relay
- Relays forward broadcasts between regions

### hang-cli

A command-line tool for:

- Publishing media from files or streams
- Using FFmpeg as input source
- Testing and development
- Media server deployments

### Browser Libraries

TypeScript packages for web applications:

- `@moq/lite` - Core protocol implementation
- `@moq/hang` - Media library with Web Components
- `@moq/hang-ui` - UI components (SolidJS)

### Native Libraries

Rust crates for server-side and native apps:

- `moq-lite` - Core protocol implementation
- `hang` - Media library
- `moq-relay` - Relay server
- `moq-native` - QUIC endpoint helpers
- `libmoq` - C bindings

## Design Principles

### 1. Simplicity

Each layer does one thing well:
- QUIC handles networking
- moq-lite handles pub/sub
- hang handles media

### 2. Generality

moq-lite works for any live data:
- Video streaming
- Audio conferencing
- Text chat
- Sensor data
- Game state
- Collaborative editing

### 3. Deployability

The relay is simple enough to:
- Run on commodity hardware
- Scale horizontally
- Deploy globally
- Operate without media expertise

### 4. End-to-End

Applications control:
- Media formats and quality
- Encryption and security
- Business logic
- User experience

The relay just moves bytes efficiently.

## Comparison to Other Protocols

### vs WebRTC

**Similarities:**
- Real-time latency
- QUIC/UDP based
- Browser support

**Differences:**
- MoQ: Fan-out via relay (1-to-many)
- WebRTC: Peer-to-peer (1-to-1) or SFU (complex)
- MoQ: Application controls media pipeline
- WebRTC: Browser controls media pipeline

### vs HLS/DASH

**Similarities:**
- Adaptive bitrate
- Wide compatibility

**Differences:**
- MoQ: Real-time latency (< 1 second)
- HLS/DASH: High latency (5-30 seconds)
- MoQ: Live-first design
- HLS/DASH: VOD-first design

### vs RTMP/SRT

**Similarities:**
- Low latency
- Live streaming

**Differences:**
- MoQ: Native browser support
- RTMP/SRT: Requires plugins or native apps
- MoQ: Modern, layered design
- RTMP/SRT: Monolithic, dated

## Next Steps

- Read the [Protocol specifications](/guide/protocol)
- Learn about [Authentication](/guide/authentication)
- Deploy to production with the [Deployment guide](/guide/deployment)
- Build with [Rust libraries](/rust/) or [TypeScript libraries](/typescript/)
