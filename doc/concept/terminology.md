---
title: Terminology
description: A glossary of MoQ terms and concepts
---

# Terminology

MoQ has its own vocabulary. Here's a quick reference for the terms you'll encounter.

## Protocols

### MoqTransport
The generic pub/sub protocol that powers everything.
It doesn't know anything about media - it just moves bytes around really efficiently.
CDNs implement this layer, and it's designed to scale to millions of subscribers.

- [More Information](/concept/standard/moq-transport)
- [Latest Draft](https://www.ietf.org/archive/id/draft-ietf-moq-transport-16.html)

### moq-lite
A stripped down subset of MoqTransport.
There's a lot of underbaked features in the IETF draft that are either useless or outright broken.
Instead of returning errors when these features are used (like other MoqTransport implementations), we've removed them entirely.

- [More Information](/concept/layer/moq-lite)
- [Latest Draft](https://www.ietf.org/archive/id/draft-lcurley-moq-lite-02.html)

### hang

The media layer that sits on top of moq-lite or MoqTransport.
It consists of a live catalog (based on WebCodecs) and a simple container for each frame.

- [More Information](/concept/layer/hang)
- [Latest Draft](https://www.ietf.org/archive/id/draft-lcurley-moq-hang-01.html)


## Core Concepts

### Broadcast
A named collection of related tracks produced by a single publisher, often with synchronized timestamps.

For example, a sports broadcast might contain:
- `catalog` - a JSON description of the other tracks
- `video` - one or more video feeds
- `audio` - one or more audio feeds
- `scoreboard` - live score updates as data

Publishers announce the availability of each broadcast.
Each subscriber can (live) learn about the existence of each broadcast, subscribing to them as needed.
This built-in gossip protocol makes it easy to implement content discovery, such as conference rooms.

### Namespace
What MoqTransport calls a `Broadcast`, with some subtle differences:
- Not always a single publisher (!)
- Identified via an array of an array of bytes (not a string)

### Track
A named and independent stream of data split into groups.

The subscriber chooses individual tracks that they want to receive.
For example, you can choose to unsubscribe to the video track when the element is hidden, without impacting audio.

Tracks are append-only.
You can't go back and change history.

### Group
A sequential collection of frames.
A track is split into groups at join points, such as keyframes, where new subscriptions can start.

Groups are the unit of reliability in MoQ and based on video GoPs.
If you're behind, you can skip the remainder of a group, not individual frames.

For example, the simplest JSON encoding is a group for each update, consisting of a single frame.
A more advanced encoding could use [JSON Merge Patch](https://datatracker.ietf.org/doc/html/rfc7386) to write each update as a delta frame.

In moq-lite this is a single QUIC stream.
In MoqTransport, each group can be split into multiple QUIC streams based on the sub-group.

### Sub-Group
*This is a MoqTransport-only concept.*

Layered encodings (SVC) split a group into multiple layers.
This is done to allow dropping the quality or frame rate instead of the tail of the group.

For example, a SVC video might be split into multiple layers:
- `video-360p` - the first layer of the SVC encoding
- `video-1080p` - the second layer of the SVC encoding
- `video-4k` - the third layer of the SVC encoding

- In moq-lite, each of these layers are separate tracks so its easier to select/prioritize them.
- In MoqTransport, each layer could instead be given a sub-group ID and delivered as a separate QUIC stream.

### Frame
The smallest unit in MoQ: a sized chunk of data.

This is typically a single media frame (audio or video) but it's up to the application.
For example, a fMP4 fragment might consist of multiple media frames (introducing latency), but it's only one frame in MoQ.

### Object
What MoqTransport calls a `Frame`.

Objects also contain a sequence number (may have gaps) and arbitrary parameters.

## Video Terms

### GoP (Group of Pictures)
A video term that maps nicely to MoQ groups.
A GoP starts with a keyframe (I-frame) and contains dependent frames (P-frames, B-frames) until the next keyframe.

Each video GoP should be delivered as a single MoQ group.
Otherwise, new subscribers won't be able to start at keyframe boundaries.

### Keyframe
A self-contained video frame that can be decoded without reference to other frames.
Viewers can only join at keyframes, which is why GoP alignment matters for latency.

## Media Components
### Catalog
A special track (usually named `catalog.json`) that contains JSON describing all the other tracks in a broadcast.
It's how viewers discover what's available and what codecs are in use.

```json
{
  "tracks": [
    { "name": "video", "codec": "avc1.640028" },
    { "name": "audio", "codec": "opus" }
  ]
}
```

### Container
In `hang`, each frame is wrapped with a timestamp and delivered as raw codec bitstream.
This simple container format works directly with WebCodecs without any demuxing.

## Network Terms

### Relay
A server that routes broadcasts between publishers and subscribers.
Relays cache recent groups, deduplicate traffic, and can cluster together for geographic distribution.

### Cluster
Multiple relays working together to form a CDN.
Publishers connect to one relay, subscribers connect to their nearest relay, and the relays route traffic between themselves.
