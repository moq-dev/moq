---
title: Concepts
description: Understanding MoQ's fundamental concepts
---

# Concepts
Welcome to my favorite section.
MoQ has been a multi-year journey to solve some very real problems in the industry and now it's time to flex the design.

## Layers
The over-arching design philosophy of MoQ is to make things simple, composable, and customizable.
We don't want you to hit a brick wall if you deviate from the standard path (*ahem* WebRTC).
We also want to benefit from economies of scale (like HTTP), utilizing generic libraries and tools whenever possible.

To accomplish this, MoQ is broken into layers:

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
│                 │     - HTTP/3 handshake
├─────────────────┤
|      QUIC       |  🌐 Underlying transport protocol
│                 │     - streams, datagrams, prioritization, etc.
└─────────────────┘
```

You get to choose which layers you want to use and which layers you want to replace.

For example, if you want to implement end-to-end encryption or a custom media format, you can fork the `hang` layer.
You still benefit from the generic `moq-lite` layer and the mass fanout (and CDN support) it provides.
If you're feeling generous, you could even contribute your changes, perhaps as a separate layer on top of `moq-lite`.

### Key Rule

**The CDN knows nothing about media.** The relay operates purely on `moq-lite` primitives (broadcasts, tracks, groups, frames) without understanding codecs, keyframes, or media containers.

This design:
- Keeps the relay simple and generic
- Enables end-to-end encryption
- Allows custom media formats
- Supports non-media use cases (chat, data streams, etc.)

## moq-lite Layer

The core transport protocol provides four primitives:

### Broadcasts

A **broadcast** is a collection of related tracks, similar to a "channel" or "room."

Example: A video conference broadcast might contain:
- `alice/video` track
- `alice/audio` track
- `bob/video` track
- `bob/audio` track
- `chat` track

### Tracks

A **track** is a named stream of data split into groups.

Properties:
- Each track has a unique name within a broadcast
- Tracks are independent and can be subscribed to separately
- Tracks are append-only live streams

Example: A `video` track contains sequential groups of video frames.

### Groups

A **group** is a sequential collection of frames, usually aligned with a natural boundary.

For video:
- Typically starts with a keyframe (I-frame)
- Contains dependent frames (P-frames, B-frames)
- Allows viewers to join at group boundaries

Properties:
- Each group has a sequential ID
- Frames within a group are ordered
- Groups are delivered over independent QUIC streams

### Frames

A **frame** is a chunk of data - the smallest unit in MoQ.

Properties:
- Sized payload of bytes
- Sequential within a group
- Can have different priorities

Example: A single video frame, audio packet, or chat message.

## hang Layer

The `hang` protocol adds media-specific structure on top of `moq-lite`:

### Catalog

A special track (usually named `catalog`) contains a JSON description of available tracks:

```json
{
  "tracks": [
    {
      "name": "video",
      "codec": "avc1.640028",
      "width": 1920,
      "height": 1080
    },
    {
      "name": "audio",
      "codec": "opus",
      "sampleRate": 48000,
      "channels": 2
    }
  ]
}
```

This enables:
- Dynamic track discovery
- Codec negotiation
- Quality selection

### Container

Each frame in `hang` consists of:
- **Timestamp** - Presentation time in microseconds
- **Codec bitstream** - Raw encoded data

This simple container format works with WebCodecs and other modern media APIs.

## QUIC and WebTransport

### QUIC

The underlying transport protocol providing:

- **Streams** - Independent bidirectional channels
- **Prioritization** - Send important data first
- **Partial reliability** - Drop old frames when behind
- **Multiplexing** - Many streams over one connection
- **Security** - TLS 1.3 built-in

### WebTransport

A browser API for QUIC:

- Based on HTTP/3 handshake
- Exposes QUIC streams and datagrams
- Supported in Chromium-based browsers
- Polyfill available for other browsers

## Data Flow Example

Here's how a video stream flows through MoQ:

### Publisher Side

1. Capture video from camera or file
2. Encode using H.264/H.265/VP9/AV1
3. Split into groups (keyframe + dependent frames)
4. Wrap each frame with timestamp
5. Publish to relay using `moq-lite`

### Relay Server

1. Accept connection from publisher
2. Store recent groups in memory
3. Accept connections from subscribers
4. Forward groups to subscribers
5. Apply prioritization rules

### Subscriber Side

1. Connect to relay
2. Subscribe to broadcast and tracks
3. Receive groups as QUIC streams
4. Extract frames and timestamps
5. Decode using WebCodecs
6. Render to video element

## Publishing and Subscribing

### Publishing

```typescript
import * as Moq from "@moq/lite";

const connection = await Moq.connect("https://relay.moq.dev/anon");
const broadcast = new Moq.BroadcastProducer();
const track = broadcast.createTrack("video");

const group = track.appendGroup();
group.write(frameData);
group.close();

connection.publish("my-stream", broadcast.consume());
```

### Subscribing

```typescript
import * as Moq from "@moq/lite";

const connection = await Moq.connect("https://relay.moq.dev/anon");
const broadcast = connection.consume("my-stream");
const track = await broadcast.subscribe("video");

for await (const group of track) {
  for await (const frame of group) {
    // Process frame
  }
}
```

## Priority and Reliability

MoQ leverages QUIC's features for optimal delivery:

### Prioritization

- **Keyframes** - Highest priority (can't decode without them)
- **Recent frames** - Higher priority than old frames
- **Audio** - Often higher priority than video

### Partial Reliability

When network is congested:
- Old frames can be skipped/dropped
- Always send the latest data
- Maintain real-time latency

This is configured at the group level in `moq-lite`.

## End-to-End Encryption

Since the relay doesn't inspect media:

- Frames can be encrypted end-to-end
- Relay only sees group/frame structure
- Application handles key exchange
- Perfect for private communications

## Non-Media Use Cases

The generic design supports:

- **Text chat** - Each message is a frame
- **Data streams** - Sensor data, logs, metrics
- **Collaborative editing** - Operation streams
- **Gaming** - Player positions, events

## Next Steps

- Read the [Protocol specs](/concept/layer/)
- Learn about [Authentication](/app/relay/auth)
- Compare with [other protocols](/concept/use-case/contribution)
- Try the [Rust libraries](/rs/)
- Try the [TypeScript libraries](/js/)
