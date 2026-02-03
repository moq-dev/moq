---
title: MoQ vs WebRTC
description: How MoQ compares to conferencing protocols like WebRTC
---

# MoQ vs WebRTC

This page compares MoQ with **WebRTC**, the dominant protocol for real-time browser communication and video conferencing.

## What is WebRTC?

WebRTC (Web Real-Time Communication) is a browser API and protocol suite for peer-to-peer audio, video, and data communication. It powers:

- Video conferencing (Zoom, Google Meet, Teams)
- VoIP applications
- Peer-to-peer file sharing
- Live streaming (some implementations)

WebRTC was designed for **bidirectional, low-latency communication** between peers.

## Comparison Table

| Feature | WebRTC | MoQ |
|---------|--------|-----|
| **Primary Use** | Conferencing | Live streaming |
| **Connection Model** | Peer-to-peer / SFU | Relay-based |
| **Browser Support** | Excellent | WebTransport required |
| **Latency** | <100ms typical | <100ms - 1s |
| **Scale** | 10s-100s per session | Millions |
| **Media Control** | Browser-controlled | App-controlled |
| **NAT Traversal** | STUN/TURN/ICE | WebTransport (HTTP/3) |
| **Encryption** | DTLS-SRTP | QUIC TLS 1.3 |
| **Codec Negotiation** | SDP | Application-level |

## Architecture Differences

### WebRTC Architecture

```text
┌────────┐     ┌────────┐
│ Peer A │◄───►│ Peer B │  (Peer-to-Peer)
└────────┘     └────────┘

┌────────┐     ┌─────┐     ┌────────┐
│ Peer A │◄───►│ SFU │◄───►│ Peer B │  (With SFU)
└────────┘     └─────┘     └────────┘
                 ↕
              ┌────────┐
              │ Peer C │
              └────────┘
```

- P2P: Direct connection between peers
- SFU: Selective Forwarding Unit routes media between many peers
- MCU: Mixing server (less common now)

### MoQ Architecture

```text
┌───────────┐     ┌───────┐     ┌────────────┐
│ Publisher │────►│ Relay │────►│ Subscriber │
└───────────┘     └───────┘     └────────────┘
                      │
                      ▼
                 ┌───────┐
                 │ Relay │  (CDN mesh)
                 └───────┘
                      │
                      ▼
               ┌────────────┐
               │ Subscriber │
               └────────────┘
```

- Publisher-subscriber model
- Relays form CDN-like mesh
- Clear separation of roles

## Key Differences

### 1. Connection Model

**WebRTC:** Designed for bidirectional peer communication
- Each participant is both sender and receiver
- Connection establishment is complex (ICE, STUN, TURN)
- SFU needed for more than a few participants

**MoQ:** Designed for broadcast/multicast
- Clear publisher and subscriber roles
- Simple connection (WebTransport over HTTP/3)
- Natural fan-out via relays

### 2. Media Pipeline Control

**WebRTC:** Browser controls the media pipeline
- Browser decides codec, bitrate, resolution
- Limited application control
- Simulcast for multi-quality (complex)
- SVC support varies

**MoQ:** Application controls the media pipeline
- App decides codec, bitrate, resolution via WebCodecs
- Full control over encoding/decoding
- Multiple tracks for different qualities
- Any codec WebCodecs supports

### 3. Scalability

**WebRTC:**
- P2P: 2-4 participants max
- SFU: 10-100 participants typical
- Each new viewer requires SFU resources
- Complex mesh for large meetings

**MoQ:**
- Designed for 1-to-millions
- Relay fan-out is efficient
- CDN-like scaling patterns
- Resource usage scales with publishers, not viewers

### 4. Latency Characteristics

**WebRTC:**
- Optimized for <100ms latency
- Jitter buffers typically 50-150ms
- Aggressive packet loss concealment
- Designed for real-time conversation

**MoQ:**
- Configurable latency
- Can be <100ms for real-time
- Can buffer more for better quality
- Prioritization over reliability

### 5. NAT Traversal

**WebRTC:**
- Complex ICE negotiation
- STUN servers for discovery
- TURN servers for relay fallback
- Connection setup takes seconds

**MoQ:**
- WebTransport uses HTTP/3
- Works through most firewalls
- No special traversal needed
- Connection setup is fast

### 6. Browser Support

**WebRTC:**
- All major browsers
- Mature and stable
- getUserMedia API for capture
- RTCPeerConnection API

**MoQ:**
- Requires WebTransport (Chrome, Edge)
- Firefox/Safari: experimental or coming
- WebCodecs for encoding/decoding
- Newer APIs, less mature

## Use Case Analysis

### Video Conferencing (2-10 people)

**WebRTC is better:**
- Designed for this use case
- Bidirectional is natural
- Low latency by default
- Mature ecosystem

**MoQ approach:**
- Each participant publishes
- Each participant subscribes to others
- Works, but more complex for small groups

### Large Meeting (10-1000 people)

**WebRTC with SFU:**
- SFU routes media between participants
- Works well for known participant sets
- Resource-intensive for many active speakers

**MoQ:**
- Natural fit for few speakers, many viewers
- Efficient fan-out
- Viewers can subscribe to speakers they want

### Live Streaming (1 to many)

**WebRTC:**
- Not designed for this
- SFU becomes bottleneck
- Often combined with HLS fallback
- Complex infrastructure

**MoQ is better:**
- Designed for broadcast
- Natural relay fan-out
- CDN-like scaling
- Simpler architecture

### Interactive Live Streaming

**WebRTC:**
- Good for bringing viewers "on stage"
- Complex when mixing models
- Often hybrid WebRTC + HLS

**MoQ:**
- All participants use same protocol
- Easy to promote viewer to speaker
- Data channels for chat built-in

## Technical Comparison

### Codec Handling

**WebRTC:**
```javascript
// Browser negotiates codecs
const pc = new RTCPeerConnection();
// Codecs determined during SDP exchange
// Limited control
```

**MoQ with WebCodecs:**
```javascript
// Full control over encoding
const encoder = new VideoEncoder({
    output: (chunk) => sendToMoQ(chunk),
});
encoder.configure({
    codec: 'avc1.42001f',
    width: 1280,
    height: 720,
    bitrate: 2_500_000,
});
```

### Connection Setup

**WebRTC:**
1. Create offer
2. ICE candidate gathering
3. STUN/TURN queries
4. Answer exchange
5. ICE connectivity checks
6. DTLS handshake
7. Media flow begins

**MoQ:**
1. WebTransport connect (HTTP/3)
2. MoQ session established
3. Announce/Subscribe
4. Media flow begins

### Error Recovery

**WebRTC:**
- FEC (Forward Error Correction)
- NACK-based retransmissions
- PLI (Picture Loss Indication)
- Concealment at decoder

**MoQ:**
- QUIC reliability per-stream
- Prioritization (drop old, send new)
- Application-level recovery
- Frame-level control

## When to Use What

### Use WebRTC when:
- Building video conferencing
- All participants send and receive
- Group size is small (<100)
- You need mature browser support
- Lowest latency is critical

### Use MoQ when:
- Building live streaming
- Clear publisher/viewer roles
- Scale to thousands or millions
- You want media pipeline control
- End-to-end delivery (no HLS fallback)

### Consider Both when:
- Interactive live streaming
- Webinars with Q&A
- Hybrid use cases

## Migration & Interop

### Using Both Protocols

Many applications can benefit from both:

```
Conferencing:
  Host ↔ WebRTC ↔ Guest (small group, bidirectional)

Broadcasting:
  Host → MoQ → Relay → Viewers (large audience, unidirectional)

Promotion:
  Viewer → WebRTC → Host (bring on stage)
```

### MoQ for WebRTC-like Use Cases

MoQ can implement conferencing patterns:
- Each participant publishes their streams
- Each participant subscribes to others
- Relay handles routing
- Works, but different trade-offs

## Summary

| Use Case | Winner |
|----------|--------|
| 1-on-1 calls | WebRTC |
| Small meetings | WebRTC |
| Large meetings | MoQ or hybrid |
| Live streaming | MoQ |
| Interactive streaming | MoQ |
| Broadcast to millions | MoQ |

WebRTC excels at **real-time bidirectional communication** between small groups.

MoQ excels at **scalable live delivery** from publishers to viewers.

## Next Steps

- Compare with [contribution protocols](/concept/use-case/contribution) (RTMP/SRT)
- Compare with [distribution protocols](/concept/use-case/distribution) (HLS/DASH)
- Read the [Protocol specification](/concept/layer/)
- Try the [Quick Start](/setup/)
