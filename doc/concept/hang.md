---
title: hang
description: The media layer, a WebCodecs-shaped catalog plus timestamped frames
---

# hang

hang is the media format on top of [moq-lite](/concept/moq-lite): a catalog
track that describes the media tracks, and a container that gives each frame a
timestamp. It is modeled on [WebCodecs](https://www.w3.org/TR/webcodecs/) so a
browser can decode it directly. The spec is
[draft-lcurley-moq-hang](/draft/moq-hang). Broadcast names end in `.hang` so
a player knows which catalog to expect.

## Catalog

`catalog.json` is a JSON track listing the renditions of each media kind and
the decoder config for each. It updates live as tracks come and go, and a
compressed twin (`catalog.json.z`) is published alongside it.

```json
{
  "video": {
    "renditions": {
      "hd": {
        "codec": "avc1.64001f",
        "description": "0164001f...",
        "codedWidth": 1280,
        "codedHeight": 720,
        "container": { "kind": "legacy" }
      }
    }
  },
  "audio": {
    "renditions": {
      "en": { "codec": "opus", "sampleRate": 48000, "numberOfChannels": 2, "container": { "kind": "legacy" } }
    }
  }
}
```

Each rendition extends the WebCodecs `VideoDecoderConfig` or
`AudioDecoderConfig`. Every WebCodecs codec is fair game; H.264, H.265, VP8,
VP9, AV1, AAC, and Opus are what the tools produce today. Properties shared by
every video rendition (display size, rotation, flip) sit at the section root.

A few things the catalog can express beyond decoder config:

- **Renditions in another broadcast.** A rendition may point at a relative broadcast path, so a transcoder can publish a ladder that adds low rungs and references the source's original rendition without re-publishing its bytes.
- **Stalled renditions.** A publisher can flag a rendition as temporarily bad so players prefer another one without the track disappearing.
- **Timelines.** A rendition may name a small timeline track mapping every group to its start time, which is what lets the [HLS gateway](/bin/hls) build playlists without subscribing to media.
- **Extensions.** The root is a loose object. Applications add their own sections (`scte35`, `chat`, `transcript`) next to `video` and `audio`, optionally naming a track that carries the data. Every library exposes a way to write your section without clobbering the media ones, and readers ignore what they don't know.

## Container

The `container.kind` on each rendition says how frames are framed:

| Kind | Frame layout | Use |
| --- | --- | --- |
| `legacy` | varint microsecond timestamp + codec payload | The default. Cheapest. |
| `cmaf` | `moof` + `mdat` | fMP4 passthrough for HLS/DASH interop; ~100 bytes per frame. |
| `loc` | small property block + payload | The IETF [LOC](/concept/standard#loc) container. |

A consumer skips renditions with a kind it doesn't recognize and carries them
through when republishing the catalog.

## Groups and keyframes

A video group is a GoP: it begins with a keyframe and holds the frames that
depend on it. That alignment is what makes MoQ's congestion behavior safe. A
relay can drop a whole group, a viewer can join at any group boundary, and the
decoder never sees a frame whose reference is missing. Audio groups are
independent too and typically hold about a second.

The `description` field carries out-of-band codec setup (an `avcC` box for
H.264). When it is absent, the parameter sets ride inline before each keyframe,
which is what `avc3`/`hev1` tracks do. Decoders should handle both.

## Your own format

hang is a convention, not a requirement. If you control both ends, publish
whatever frames you like on raw tracks; the relay never looks inside them. The
[MoQ Boy](/bin/demo) demo mixes hang media tracks with raw JSON status and
command tracks on the same broadcast.
