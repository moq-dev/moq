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

- **Labels.** Any rendition may carry a human-readable `label` for a track picker. The map key stays the track name used to subscribe, so labels need not be unique and renaming one doesn't rename the track.
- **Renditions in another broadcast.** A rendition may point at a relative broadcast path, so a transcoder can publish a ladder that adds low rungs and references the source's original rendition without re-publishing its bytes. The path resolves against where the consumer found the catalog, so a reference that escapes above the root names nothing and the catalog is rejected.
- **Stalled renditions.** A publisher can flag a rendition as temporarily bad so players prefer another one without the track disappearing.
- **Timelines.** A broadcast may publish a small timeline track logging each complete segment, aligned across renditions, which is what lets the [HLS gateway](/bin/hls) build playlists without subscribing to media.
- **Extensions.** The root is a loose object. Applications add their own sections (`scte35`, for example) next to the ones hang defines, optionally naming a track that carries the data. Every library exposes a way to write your section without clobbering the built-in ones, and readers ignore what they don't know.

## Text

Captions and subtitles are their own tracks in a `text` section, not part of
the video bitstream, so the relay stays media-agnostic and a viewer downloads
only the language it picked. Renditions work like audio: usually one per
language, with `lang` and `label` driving the picker.

There is no WebCodecs decoder for text, so a consumer parses each cue itself.
`format` says how (`vtt`, `ttml`, or `utf8`) and `role` is `subtitle` (dialogue)
or `caption` (all audio). Each frame's timestamp is the cue's start time on the
same media clock, so cues schedule against the same playhead.

## Data tracks

Not everything in a broadcast is media: a chat log, a telemetry feed, a
thumbnail, a serialized game state. The `json` and `binary` sections list these
as plain tracks, split by whether a generic consumer can parse the payload.

```json
{
  "json": {
    "tracks": {
      "chat": { "mode": "stream", "compression": "deflate" },
      "status": { "mode": "snapshot" }
    }
  },
  "binary": {
    "tracks": {
      "thumbnail": { "mode": "snapshot", "mime": "image/jpeg" }
    }
  }
}
```

Unlike the media sections these are not renditions: each entry is a distinct
track, not an alternative to choose between. `mode` is required and says how
groups compose the frames, because reading an append log as a latest-value
document would silently discard everything but the last payload:

- `snapshot` is lossy. Each group supersedes the previous one, so a consumer reads only the newest. A JSON track may follow the first frame with merge-patch deltas.
- `stream` is an ordered log: one payload per frame, all in a single group that is never rolled. Retention is still bounded by the group cache, and a consumer that falls behind fails the read rather than silently resuming mid-log.

The rest is descriptive: `compression` (`deflate`, the same group-scoped
`deflate-raw` the catalog uses), `schema` on a JSON track, `mime` on a binary
one, plus the `broadcast` and `timeline` fields a media rendition takes. A
consumer that doesn't recognize a `mode` or `compression` ignores that track and
round-trips it verbatim.

In Rust the catalog owns the lifetime: `catalog.json_stream(track, config)` (or
`json_snapshot` / `binary_snapshot` / `binary_stream`) writes the entry and
retracts it when the producer drops, and `catalog.json_track(name)` returns an
entry that subscribes itself. In the browser, read the entry from
`catalog.json.tracks`, subscribe by name, and hand the track to `@moq/json` or
`@moq/binary`.

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
[MoQ Boy](/bin/demo) demo mixes hang media tracks with JSON status and command
tracks on the same broadcast.
