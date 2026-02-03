---
title: Hang
description: A simple, WebCodecs-based media format utilizing MoQ.
---

# hang
A simple, WebCodecs-based media format utilizing MoQ.

See the draft: [draft-lcurley-moq-hang](https://www.ietf.org/archive/id/draft-lcurley-moq-hang-01.html).

## Catalog
`catalog.json` is a special track that contains a JSON description of available tracks.
This track is live updated as tracks are added, removed, or changed.

MoQ doesn't have a mechanism to discover track names; that's the role of this catalog track.
A viewer has to know ahead of time that a broadcast contains a `catalog.json` track.

It's possible to have multiple different types/versions of catalog tracks.
For example, a `playlist.m3u8` track could be used in conjunction with a `catalog.json` track to provide backwards compatibility for HLS.

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
    },
    "priority": 1
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
    },
    "priority": 2
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

The same container formats are used for both video and audio.

### Legacy
This is a lightweight container with no frills attached.
It's called "legacy" because it's not extensible nor optimized and will be deprecated in the future.

Each frame consists of:
- A 62-bit (varint-encoded) presentation timestamp in microseconds.
- The codec payload.

### CMAF
This is a more robust container used by HLS/DASH.

Unfortunately, it's not quite designed for real-time streaming and incurs either a latency or size overhead:
- Minimal latency: 1-frame fragments introduce ~100 bytes of overhead per frame.
- Minimal size (HLS): GoP sized fragments introduce a GoP's worth of latency.
- Mixed latency/size (LL-HLS): 500ms sized fragments introduce a 500ms latency, with some additional overhead.

Each frame consists of:
- A `moof` box containing a `tfhd` box and a `tfdt` box.
- A `mdat` box containing the codec payload.
