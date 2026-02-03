---
title: MoQ vs RTMP/SRT
description: How MoQ compares to contribution protocols like RTMP and SRT
---

# MoQ vs RTMP/SRT

This page compares MoQ with traditional **contribution protocols** like RTMP and SRT, which are typically used to send live video from encoders to media servers.

## What are Contribution Protocols?

Contribution protocols handle the **first mile** of live streaming - getting content from the source (camera, encoder, OBS) to the media server. They prioritize:

- Low latency encoding-to-server
- Reliable delivery over unreliable networks
- Simple encoder integration
- Single stream per connection

Common examples:
- **RTMP** (Real-Time Messaging Protocol) - The classic Flash-era protocol
- **SRT** (Secure Reliable Transport) - Modern low-latency alternative
- **RTSP** (Real-Time Streaming Protocol) - Used in IP cameras

## Comparison Table

| Feature | RTMP | SRT | MoQ |
|---------|------|-----|-----|
| **Transport** | TCP | UDP | QUIC (UDP) |
| **Latency** | 1-5s | 120ms-2s | <100ms possible |
| **Browser Support** | None (Flash dead) | None | Native WebTransport |
| **Encryption** | RTMPS (TLS) | AES-128/256 | TLS 1.3 built-in |
| **Congestion Control** | None (TCP) | Custom | QUIC (BBR, etc.) |
| **FEC** | No | Yes | QUIC retransmits |
| **Multiplexing** | Limited | No | Native (QUIC streams) |
| **Fan-out** | Server-side only | Server-side only | Native relay support |

## Protocol Details

### RTMP

RTMP was designed by Macromedia for Flash Player in 2002.

**Pros:**
- Universal encoder support (OBS, FFmpeg, etc.)
- Well-understood and documented
- Works through most firewalls (TCP)

**Cons:**
- Flash is dead - browsers can't receive RTMP
- TCP causes head-of-line blocking
- No built-in encryption (RTMPS is optional)
- Limited to FLV container (H.264/AAC)
- High latency (1-5 seconds typical)
- No adaptive bitrate at protocol level

**Typical Use:**
```
OBS → RTMP → Media Server → Transcode → HLS/DASH → CDN → Viewers
```

### SRT

SRT was developed by Haivision and open-sourced in 2017.

**Pros:**
- UDP-based with FEC for packet loss recovery
- AES encryption built-in
- Low latency (120ms-2s configurable)
- ARQ (Automatic Repeat Request) for reliability
- Modern, actively developed

**Cons:**
- No native browser support
- Single stream per connection
- Requires port forwarding/NAT traversal
- Firewall issues (UDP)
- No built-in CDN/fan-out support

**Typical Use:**
```
Encoder → SRT → Media Server → Transcode → HLS/DASH → CDN → Viewers
```

### MoQ

MoQ is designed from the ground up for modern live streaming.

**Pros:**
- Native browser support (WebTransport)
- End-to-end delivery (source to viewer)
- Built-in relay/CDN support
- QUIC handles congestion, encryption
- Multiplexed streams (audio, video, data)
- Sub-second latency possible
- Prioritization and partial reliability

**Cons:**
- New protocol, less encoder support
- Requires QUIC/WebTransport infrastructure
- UDP may be blocked in some networks

**Typical Use:**
```
Browser/Encoder → MoQ Relay → MoQ Relay (CDN) → Browser/App
```

## Key Differences

### 1. End-to-End vs Contribution-Only

RTMP and SRT are **contribution protocols** - they get content to a server, then something else distributes it.

MoQ is **end-to-end** - the same protocol works from source to viewer, with relays in between.

```
Traditional:
  Encoder → RTMP → Server → Transcode → HLS → CDN → Player

MoQ:
  Publisher → MoQ → Relay → MoQ → Viewer
```

### 2. Browser Support

RTMP: Dead (Flash)
SRT: None
MoQ: Native via WebTransport

This means MoQ can work entirely in the browser - both publishing and viewing.

### 3. Multiplexing

RTMP: Limited (audio + video in one stream)
SRT: Single stream per connection
MoQ: Multiple independent tracks via QUIC streams

MoQ's multiplexing enables:
- Independent audio/video delivery
- Multiple quality levels
- Additional data tracks (chat, metadata)
- Per-track prioritization

### 4. Latency Architecture

RTMP: TCP reliability causes buffering
SRT: UDP with configurable latency buffer
MoQ: QUIC streams with prioritization and partial reliability

MoQ can drop old data when congested, maintaining real-time latency.

### 5. Scale Architecture

RTMP/SRT: Require a separate distribution system
MoQ: Built-in relay fan-out

MoQ relays can form a mesh/CDN without transcoding or protocol translation.

## When to Use What

### Use RTMP when:
- You need maximum encoder compatibility
- You're feeding into an existing media server (Wowza, nginx-rtmp)
- Latency of 1-5 seconds is acceptable
- You have infrastructure already built around RTMP

### Use SRT when:
- You need low latency contribution (< 1 second)
- You're sending over unreliable networks (Internet, cellular)
- You need encryption for contribution
- Firewall/NAT traversal is manageable

### Use MoQ when:
- You need browser-based publishing
- You want end-to-end low latency
- You're building real-time interactive applications
- You want to leverage a single protocol source-to-viewer
- You need CDN-like scale with low latency

## Migration Path

### From RTMP to MoQ

1. Keep RTMP ingest for legacy encoders
2. Use MoQ relay for distribution
3. Add MoQ publishing for browser-based sources
4. Gradually migrate encoders to MoQ

### From SRT to MoQ

1. SRT and MoQ can coexist
2. Use MoQ for browser-based endpoints
3. Consider MoQ for new infrastructure
4. SRT remains good for point-to-point low latency

## Example: OBS to Browser

**Traditional (RTMP + HLS):**
```
OBS → RTMP → Media Server → Transcode → HLS → CDN → Browser
Latency: 5-30 seconds
```

**With SRT:**
```
OBS → SRT → Media Server → Transcode → HLS → CDN → Browser
Latency: 3-15 seconds (contribution faster, distribution slow)
```

**With MoQ:**
```
OBS → MoQ → Relay → Browser
Latency: <1 second (or lower)
```

Note: MoQ OBS plugin is in development. See [OBS Plugin](/app/obs).

## Next Steps

- Compare with [distribution protocols](/concept/use-case/distribution) (HLS/DASH)
- Compare with [conferencing protocols](/concept/use-case/conferencing) (WebRTC)
- Read the [Protocol specification](/concept/layer/)
- Try the [Quick Start](/setup/)
