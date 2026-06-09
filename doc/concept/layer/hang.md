---
title: Hang
description: A simple, WebCodecs-based media format utilizing MoQ.
---

# hang

A simple, WebCodecs-based media format utilizing MoQ. See the [specification](https://datatracker.ietf.org/doc/draft-lcurley-moq-hang/) for wire-level details.

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

### Extensions

The base catalog carries only the media sections (`video` and `audio`).
Applications add their own root sections (for example `scte35`) without modifying hang.

The catalog is a JSON document published through the merge-patch helper (`@moq/json` / `moq-json`), and an extension is just an extra top-level key:

- **Reading**: the base schema is permissive, so unknown sections pass through validation untouched.
  A base consumer ignores them; an extension reads its own section and treats its absence as "not present".
  In TypeScript, build an extended schema with `z.extend(Catalog.RootSchema, { scte35: ... })`; in Rust, flatten the catalog into your own struct with `#[serde(flatten)]`.
- **Writing**: the catalog producer holds one shared document.
  Each owner edits only its own keys and publishes (`producer.mutate(c => { c.scte35 = ... })` in TypeScript, or the `Deref`/`DerefMut` lock guard from `producer.lock()` in Rust).
  Every edit starts from the latest value, so the base media sections and any extension sections compose instead of clobbering one another.
  Removing a key publishes a deletion, which a consumer reads as the section being removed.

This keeps application-specific sections in the application layer while the base catalog stays generic.

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
- Mixed latency/size (LL-HLS): 500ms-sized fragments introduce a 500ms latency, with some additional overhead.

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

## Custom Media Formats

You can make your own media format if you have full control over the publisher and all viewers.
You would be missing out on existing tools and libraries but it's really not that complicated;
QUIC and moq-lite do the heavy lifting.
