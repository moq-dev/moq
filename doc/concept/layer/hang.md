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

### Compression

The catalog is published on two tracks with identical content: `catalog.json` (plain JSON) and `catalog.json.z` (the same JSON, DEFLATE-compressed per group).
A publisher always serves both; a consumer reads whichever it prefers and defaults to the uncompressed `catalog.json`.

The compression is the group-scoped `deflate-raw` ([RFC 1951](https://www.rfc-editor.org/rfc/rfc1951.html)) stream used by `@moq/json` / `moq-json`, interoperable between the browser and native.
To read the compressed track, opt in explicitly: pass `--catalog hangz` to `moq-cli subscribe`, `CatalogFormat::HangZ` in Rust, or `catalogFormat: "hangz"` to `@moq/watch`.
The `.hang` broadcast suffix is unchanged: the compressed track is an extra track on the same broadcast, not a different broadcast name.

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
  In TypeScript, build an extended schema with `z.extend(Catalog.RootSchema, { scte35: ... })`.
  In Rust, either flatten the catalog into your own struct with `#[serde(flatten)]` for typed access, or read sections untyped from an `Extra` catalog, which keeps unknown keys as raw JSON (`catalog.section("scte35")`). The `()` default drops sections it doesn't model.
  The FFI bindings always use the untyped form, one JSON string per section keyed by name (`catalog.sections["scte35"]` in Python, `moq_catalog_get_section()` / `moq_catalog_section_at()` in C).
- **Writing**: the catalog producer holds one shared document.
  Each owner edits only its own keys and publishes: `producer.mutate(c => { c.scte35 = ... })` in TypeScript; the `Deref`/`DerefMut` lock guard from `producer.lock()` for a typed Rust extension, or `producer.set_section("scte35", value)` for an untyped one; `broadcast.set_catalog_section("scte35", json)` in Python; `moq_publish_catalog_section()` in C.
  Every edit starts from the latest value, so the base media sections and any extension sections compose instead of clobbering one another.
  Removing a key publishes a deletion (`producer.remove_section(...)`, `broadcast.remove_catalog_section(...)` in Python, `moq_remove_catalog_section()` in C), which a consumer reads as the section being removed.

This keeps application-specific sections in the application layer while the base catalog stays generic.

### Custom tracks

A custom catalog section can carry its payload inline (for low-rate metadata), or it can reference a separate track in the same broadcast (for a stream of data, e.g. a `meta.json` track or an SCTE-35 event track). The relay treats such a track like any other; only the publisher and consumer give it meaning.

The `@moq/publish` and `@moq/watch` components publish and subscribe to these tracks generically, with no per-application support. Each exposes a low-level track hook and re-exports `@moq/json` so the application encodes the payload itself:

- **Publish**: `broadcast.publishTrack(name, serve)` runs `serve(track, effect)` per subscriber. For JSON, serve each track from a shared track-less `Json.Producer` (the same fan-out producer the catalog uses, seeding late joiners with the latest value). Advertise the track by writing your own catalog section with `broadcast.catalog.mutate(...)`.
- **Watch**: `broadcast.subscribeTrack(name, priority, consume)` follows the active broadcast across reconnects. For JSON, wrap the track in a `Json.Consumer` inside `consume`. Read your section back from `broadcast.catalog` (unknown sections pass through the loose schema).

So an application supports something like SCTE-35 entirely in its own code: publish an `scte35` section (and optionally a track) on one side, read it on the other, without hang, `@moq/publish`, or `@moq/watch` knowing anything about SCTE-35.

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
