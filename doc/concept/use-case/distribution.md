---
title: MoQ vs HLS/DASH
description: How MoQ compares to distribution protocols like HLS and DASH
---

# MoQ vs HLS/DASH

This page compares MoQ with traditional **distribution protocols** like HLS and DASH, which are used to deliver live and on-demand video to viewers at scale.

## What are Distribution Protocols?

Distribution protocols handle the **last mile** of streaming - getting content from servers to viewers. They prioritize:

- Massive scale (millions of viewers)
- CDN compatibility
- Device compatibility
- Adaptive bitrate
- VOD and live support

Common examples:
- **HLS** (HTTP Live Streaming) - Apple's protocol, dominant on iOS
- **DASH** (Dynamic Adaptive Streaming over HTTP) - MPEG standard
- **CMAF** (Common Media Application Format) - Unified container for HLS/DASH

## Comparison Table

| Feature | HLS | DASH | MoQ |
|---------|-----|------|-----|
| **Transport** | HTTP/TCP | HTTP/TCP | QUIC/UDP |
| **Latency** | 6-30s (LL-HLS: 2-5s) | 6-30s (LL-DASH: 2-5s) | <1s possible |
| **Segment Size** | 2-10s typical | 2-10s typical | Frame-level |
| **Browser Support** | Native (Safari), MSE | MSE only | WebTransport |
| **CDN Support** | Excellent | Excellent | Growing |
| **Live Focus** | VOD-first | VOD-first | Live-first |
| **Interactivity** | Limited | Limited | Real-time |

## Protocol Details

### HLS (HTTP Live Streaming)

HLS was created by Apple in 2009 for iOS devices.

**How it works:**
1. Server segments video into chunks (2-10 seconds)
2. Creates playlist files (.m3u8) listing segments
3. Client polls playlist for new segments
4. Downloads and plays segments in sequence

**Pros:**
- Universal device support
- Works with any HTTP CDN
- Adaptive bitrate built-in
- Well-understood at scale

**Cons:**
- High latency (segment duration + buffer)
- Polling adds latency and server load
- VOD-first design, live is an afterthought
- Complex encoder/packager requirements

**Low-Latency HLS (LL-HLS):**
- Partial segments and preload hints
- Reduces latency to 2-5 seconds
- More complex infrastructure

### DASH (Dynamic Adaptive Streaming over HTTP)

DASH is an international standard (ISO/IEC 23009-1) from 2012.

**How it works:**
- Similar segment-based approach to HLS
- Uses MPD (Media Presentation Description) manifests
- More flexible than HLS in some ways

**Pros:**
- Industry standard
- DRM support
- Codec flexibility
- Adaptive bitrate

**Cons:**
- Same latency issues as HLS
- More complex than HLS
- Requires Media Source Extensions in browsers
- No native iOS support

**Low-Latency DASH:**
- Similar techniques to LL-HLS
- Chunked transfer encoding
- 2-5 second latency achievable

### MoQ

MoQ takes a fundamentally different approach.

**How it works:**
1. Publisher sends frames as they're encoded
2. Relay forwards frames immediately to subscribers
3. No segmentation, no polling
4. QUIC handles reliability and ordering

**Pros:**
- Sub-second latency is normal
- Live-first design
- Real-time interactivity
- Native browser support (WebTransport)
- No complex packaging

**Cons:**
- New protocol, less infrastructure
- CDN support still developing
- Different scaling patterns than HTTP

## Key Differences

### 1. Latency Architecture

**HLS/DASH:** Segment-based polling
```
Encoder → Packager → Segments → CDN → Poll → Player
         [2-10s]              [poll interval]
```

**MoQ:** Frame-level push
```
Encoder → Relay → Player
         [frame time]
```

The segment model inherently adds latency. Even with LL-HLS/DASH, you're limited to 2-5 seconds due to buffering requirements.

### 2. Content Delivery Model

**HLS/DASH:** Pull-based
- Client polls server for new content
- Each segment is an HTTP request
- CDN caches segments

**MoQ:** Push-based
- Server pushes frames to clients
- Single persistent connection
- Relay caches recent groups

### 3. Adaptive Bitrate

**HLS/DASH:**
- Multiple quality renditions encoded in parallel
- Client switches between renditions based on bandwidth
- Smooth transitions via buffering

**MoQ:**
- Multiple tracks can exist in a broadcast
- Client subscribes to desired tracks
- Can switch tracks frame-by-frame

### 4. CDN Architecture

**HLS/DASH:**
- Works with existing HTTP CDNs
- Segments are cacheable objects
- Well-understood at scale

**MoQ:**
- Requires relay infrastructure
- Relays form a mesh/CDN
- Different caching model (recent groups)

### 5. Interactivity

**HLS/DASH:**
- Not designed for interactivity
- 5-30 second delay makes chat awkward
- Real-time features require separate channels

**MoQ:**
- Built for interactivity
- Same protocol for video and data
- Real-time chat, reactions, etc.

## Latency Comparison

| Protocol | Typical Latency | Best Case |
|----------|----------------|-----------|
| HLS | 15-30 seconds | 6 seconds |
| LL-HLS | 4-8 seconds | 2 seconds |
| DASH | 15-30 seconds | 6 seconds |
| LL-DASH | 4-8 seconds | 2 seconds |
| MoQ | 100-500ms | <100ms |

## When to Use What

### Use HLS/DASH when:
- You need maximum device compatibility
- You have existing HTTP CDN infrastructure
- Latency of 5+ seconds is acceptable
- You're primarily serving VOD content
- Scale is the top priority

### Use MoQ when:
- You need sub-second latency
- Interactivity is important (chat, sports betting, auctions)
- You want a unified live streaming stack
- You're building real-time applications
- Browser-based publishing is needed

## Migration Path

### Hybrid Approach

Many deployments can benefit from both:

```
Publisher → MoQ → Relay → MoQ viewers (real-time)
                      ↓
                   HLS packager → CDN → HLS viewers (scale)
```

- MoQ for real-time viewers (lower latency, lower scale)
- HLS/DASH for mass distribution (higher latency, higher scale)

### Gradual Migration

1. Start with HLS/DASH for distribution
2. Add MoQ for interactive features
3. Expand MoQ as infrastructure matures
4. Reduce HLS/DASH as MoQ scales

## Use Case Examples

### Live Sports

**Traditional:**
```
Camera → RTMP → Transcoder → HLS → CDN → Viewers (10-30s delay)
```
Problem: Viewers hear neighbors celebrate before seeing the goal.

**With MoQ:**
```
Camera → MoQ → Relay network → Viewers (<1s delay)
```

### Live Shopping

**Traditional (HLS):**
- Host shows product, waits 10 seconds for reaction
- Chat is awkward with delay
- "Buy now" button timing is off

**With MoQ:**
- Real-time interaction between host and viewers
- Chat feels natural
- Synchronized buying experience

### Gaming/Esports

**Traditional:**
- Twitch uses RTMP ingest + HLS distribution
- 5-15 second delay typical
- "Stream sniping" is a concern

**With MoQ:**
- Sub-second possible
- Enables new interactive features
- Better sync with game events

## Technical Details

### Segment vs Frame Delivery

HLS segment:
```
[------ 4 second segment ------]
[keyframe][p][p][p][p][p]...[p]
```
Must wait for entire segment before delivery.

MoQ group:
```
[keyframe] → immediately sent
[p-frame]  → immediately sent
[p-frame]  → immediately sent
```
Each frame sent as soon as encoded.

### CDN Caching

**HLS/DASH:**
- Segment is a file on CDN
- Cache for segment duration
- Easy to scale

**MoQ:**
- Group is cached in relay memory
- Relay decides what to keep
- Different scaling model

## Next Steps

- Compare with [contribution protocols](/concept/use-case/contribution) (RTMP/SRT)
- Compare with [conferencing protocols](/concept/use-case/conferencing) (WebRTC)
- Read the [Protocol specification](/concept/layer/)
- Try the [Quick Start](/setup/)
