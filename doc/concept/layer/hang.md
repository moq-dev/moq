---
title: Hang
description: A simple, WebCodecs-based media format utilizing MoQ.
---

# hang
A simple, WebCodecs-based media format utilizing MoQ.

See the draft: [draft-lcurley-moq-hang](https://www.ietf.org/archive/id/draft-lcurley-moq-hang-01.html).

## Catalog
`catalog.json` is a special track that contains a JSON description of available tracks.
This is how the viewer decides what it can decode and wants to receive.
The catalog track is live updated as media tracks are added, removed, or changed.

Each media track is described using the [WebCodecs specification](https://www.w3.org/TR/webcodecs/) and we plan to support every codec in the [WebCodecs registry](https://w3c.github.io/webcodecs/codec_registry.html).

### Example
Here is Big Buck Bunny's `catalog.json` as of 2026-02-02:

```json
{
  "video": {
    "renditions": {
      "video0": {
        "codec": "avc1.64001f",
        "description": "0164001fffe100196764001fac2484014016ec0440000003004000000c23c60c9201000568ee32c8b0",
        "codedWidth": 1280,
        "codedHeight": 720,
        "container": "legacy"
      }
    }
  },
  "audio": {
    "renditions": {
      "audio1": {
        "codec": "mp4a.40.2",
        "sampleRate": 44100,
        "numberOfChannels": 2,
        "bitrate": 283637,
        "container": "legacy"
      }
    }
  }
}
```

### Audio
[See the latest schema](https://github.com/moq-dev/moq/blob/main/js/hang/src/catalog/audio.ts).

Audio is split into multiple renditions that should all be the same content, but different quality/codec/language options.

Each rendition is an extension of [AudioDecoderConfig](https://www.w3.org/TR/webcodecs/#audio-decoder-config).
This is the minimum amount of information required to initialize an audio decoder.


### Video
[See the latest schema](https://github.com/moq-dev/moq/blob/main/js/hang/src/catalog/video.ts).

Video is split into multiple renditions that should all be the same content, but different quality/codec/language options.
Any information shared between multiple renditions is stored in the root.
For example, it's not possible to have a different `flip` or `rotation` value for each rendition,

Each rendition is an extension of [VideoDecoderConfig](https://www.w3.org/TR/webcodecs/#video-decoder-config).
This is the minimum amount of information required to initialize a video decoder.


## Container
The catalog also contains a `container` field for each rendition used to denote the encoding of each track.
Unfortunately, the raw codec bitstream lacks timestamp information so we need some sort of container.

Containers can support additional features and configuration.
For example, `CMAF` specifies a timescale instead of hard-coding it to microseconds like `legacy`.

### Legacy
This is a lightweight container with no frills attached.
It's called "legacy" because it's not extensible nor optimized and will be deprecated in the future.

Each frame consists of:
- A 62-bit (varint-encoded) presentation timestamp in microseconds.
- The codec payload.

### CMAF
This is a more robust container used by HLS/DASH.

Each frame consists of:
- A `moof` box containing a `tfhd` box and a `tfdt` box.
- A `mdat` box containing the codec payload.

Unfortunately, fMP4 is not quite designed for real-time streaming and incurs either a latency or size overhead:
- Minimal latency: 1-frame fragments introduce ~100 bytes of overhead per frame.
- Minimal size (HLS): GoP sized fragments introduce a GoP's worth of latency.
- Mixed latency/size (LL-HLS): 500ms sized fragments introduce a 500ms latency, with some additional overhead.

## Video `description` Field

The `description` field in a video rendition controls how codec initialization data (e.g. H.264 SPS/PPS) is delivered.
This follows the [WebCodecs AVC codec registration](https://w3c.github.io/webcodecs/avc_codec_registration.html).

### With `description` (AVCC format)

When `description` contains a hex-encoded value, the codec data is in AVCC format:
- The description contains the `avcC` box (SPS/PPS for H.264, VPS/SPS/PPS for H.265).
- NAL units in each frame payload are **length-prefixed** (4-byte big-endian length).
- The decoder is initialized once using the description, before any frames arrive.

This is the format used in the [Big Buck Bunny example](#example) above.

### Without `description` (Annex B format)

When `description` is absent or null, the codec data is in Annex B format:
- SPS/PPS are delivered **inline** in the bitstream before each keyframe.
- NAL units use start codes (`00 00 00 01`) as delimiters.
- The decoder extracts parameters from the bitstream itself.

::: tip
A missing `description` is **not** an error — it simply means the publisher is using Annex B.
Your decoder should handle both formats.
:::

## Groups and Keyframes

Each MoQ group aligns with a video Group of Pictures (GoP).
A new group starts with a keyframe (IDR frame) that can be decoded independently.

This has important implications:
- **Skipping a group means skipping an entire GoP.** The relay can drop old groups without corrupting the decoder state.
- **Late-join viewers** need the first frame of a group (the keyframe) to start decoding. Joining mid-group produces corrupted output until the next group boundary.
- **Audio groups** typically align with video groups but may contain multiple audio frames per group.

The relay uses group boundaries for partial reliability: if congestion occurs, entire groups are dropped rather than individual frames, keeping the decoder in a consistent state.
