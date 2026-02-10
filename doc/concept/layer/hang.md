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

## `description`
The `description` field in audio/video renditions contains codec-specific initialization data based on the [WebCodecs codec registration](https://www.w3.org/TR/webcodecs-codec-registry/).

For example, the `description` field for [H.264](https://www.w3.org/TR/webcodecs-avc-codec-registration/) can be:
- **present**: the `description` is an `avcC` box, containing the SPS/PPS and other information.
- **absent**: the SPS/PPS NALUs are delivered **inline** before each keyframe.

There's no "right format" and both exist in the wild.
Inlining the SPS/PPS marginally increases the overhead of each frame, but it means the decoder can be reinitialized (ex. resolution change).

Unfortunately, your decoder should handle both.

## Groups and Keyframes

Each MoQ group aligns with a video Group of Pictures (GoP).
A new group starts with a keyframe (IDR frame) that can be decoded independently.

This has important implications:
- **Skipping a group means skipping an entire GoP.** The relay can drop old groups without corrupting the decoder state.
- **Late-join viewers** start at the beginning of a group (the keyframe), since it's not possible to join mid-group.
- **Audio groups** don't need to align with video groups and can contain any number of frames.

The relay uses group boundaries for partial reliability: if congestion occurs, entire groups are dropped rather than individual frames, keeping the decoder in a consistent state.
