---
title: hang
description: The media layer, a WebCodecs-shaped catalog plus timestamped frames
---

# hang

hang is the media format on top of [moq-lite](/concept/moq-lite): a catalog
track that describes the media tracks, and a container that gives each frame a
timestamp. It is modeled on [WebCodecs](https://www.w3.org/TR/webcodecs/) so a
browser can decode it directly. The spec is
[draft-lcurley-moq-hang](/draft/moq-hang). Broadcast names end in
`.hang` so a player knows which catalog to expect.

## Catalog

`catalog.json` is a JSON track listing the renditions of each media kind and
the decoder config for each. It updates live as tracks come and go, and is a
[compressed track](/concept/compression), so a reader sees plain JSON
whether or not its session carries the compression.

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

A track name always means the same content, so a catalog update never
redefines a track. The fields a decoder can't change without reinitializing
are fixed: codec, description, container, display aspect, interlacing, sample
rate, and channel count. Everything else, such as the coded size, `bitrate`,
`framerate`, and `enabled`, updates in place; the picture resizes in band at a
keyframe unless a `description` pins it. A publisher that needs a new identity
publishes a new track name, or restarts the broadcast under a new
[identity](/concept/moq-lite#publisher-identities); the Rust and `@moq/publish`
publishers take a new name. A player can configure its decoder once per track.

A few things the catalog can express beyond decoder config:

- **Labels.** Any rendition may carry a human-readable `label` for a track picker. The map key stays the rendition's ID, so labels need not be unique and renaming one doesn't rename the track.
- **Renditions in another broadcast.** A rendition may point at a relative broadcast path, so a transcoder can publish a ladder that adds low rungs and references the source's original rendition without re-publishing its bytes. The path resolves against where the consumer found the catalog, so a reference that escapes above the root names nothing and the catalog is rejected. `@moq/watch` and `moqsrc` follow a referenced path's announcements like their own, restarting the rendition on each identity that serves it; every other reader, such as `moq play`, the exporters, and the bindings, waits for the first one announced and keeps it, and an exporter ends the rendition with it. Which [stream](/concept/moq-lite#publisher-identities) serves the path comes from its announcements, never the catalog; see [identity and recovery](/concept/identity).
- **Whole includes.** A root `include` map names whole broadcasts, such as transcoded renditions of the source, by the same relative path as a rendition's reference. They share the listing catalog's PTS timeline and root settings, including its clock and join interval. Listing one opens nothing, so an include produced on demand starts only once a viewer selects it. `@moq/watch` selects every include it lists, as an HLS master does: it reads each include's catalog and offers its video and audio renditions beside the broadcast's own, as `<include>/<id>`, to the same rendition selection, subscribing media only for the rendition it picks. `broadcast.include(effect, id)` reads one include as a broadcast of its own. Its rendition IDs stay its own.
- **Track names.** The map key is a rendition's ID and, by default, its track name. A rendition sets `track` only when the two must differ, such as two renditions of same-named tracks in different broadcasts, or a rendition whose media changed and so needs a new name; readers subscribe to `track` when present. An ID can't name both a video and an audio rendition.
- **Jitter.** A rendition can say how much media it writes behind a frame's timestamp, in whole milliseconds rounded up: the reorder delay of B-frames and the span of a fragment or batch written together. It is measured from media timestamps alone, never the time a frame was written, so neither a constant encoder delay nor a capture clock counts. It describes the publisher, never the network, only grows over the life of a stream, and a player sizes its buffer to at least this much. A `0` is read as absent.
- **Delay.** A rendition can also say how far its frames' timestamps persistently trail those another rendition already wrote, so a video encoder running 200 ms behind audio advertises `delay: 200` on video. The publisher takes the least any frame trails within each window of a second or more and keeps the largest window, so lateness only some frames have, such as B-frame reorder, is left to jitter. A rendition alone measures none. It follows the same rules as jitter. A player holds the largest `delay + jitter` among the renditions it subscribes to, and never subtracts one rendition's `delay` from another's.
- **Reorder depth.** A video rendition may declare `reorder`, the most frames that can precede one in decode order and follow it in presentation, as H.264 `max_num_reorder_frames` counts them; `0` declares none, which is not the same as absent. The Rust H.264 and H.265 importers copy it from the SPS when the SPS declares one (progressive H.264 only), and `@moq/publish` declares `0`. The MPEG-TS, Matroska, and FLV exporters then derive each group's decode times from that group's frames alone, so exports that joined at different groups agree, and they end the export on a frame reordered deeper than declared or a depth the codec level cannot reach. Without it they infer decode times from the timestamps they have read, as before. A publisher never changes it once declared.
- **Colour.** A video rendition may carry `color`: the H.273 primaries, transfer, matrix, and range, plus the HDR10 mastering display and content light levels, so a renderer can set up HDR without parsing the bitstream. The Rust importers fill it from the SPS VUI, AV1 sequence header, and VP9 header, then fill gaps from a container's `colr` box or Matroska `Colour` element; the stream wins a disagreement. The HDR10 values come from H.264 and H.265 SEI or AV1 metadata OBUs in MPEG-TS and raw elementary streams, an AV1 track's av1C, or Matroska, but not yet from H.264 or H.265 in fMP4 or FLV. Nothing in `@moq/watch` reads it yet.
- **Spatial media.** A video rendition may carry `projection` (flat `rectilinear`, or `equirectangular` with the degrees of longitude it spans, 180 for VR180) and `stereo` (`mono`, `left-right`, or `top-bottom` frame packing), and an audio rendition may carry `layout: "ambisonic"` for AmbiX channels. Readers treat a value they don't recognize as absent. The Rust importers fill them from MP4 `sv3d` and `st3d`, Matroska, and Opus channel mapping family 2. `@moq/watch` plays the picture and channels as they are; the [VR demo](/bin/demo#vr) lays them out in a headset.
- **Disabled renditions.** An audio or video rendition with `enabled: false` has no new frames coming, and a player must not select it. A subscriber may still get audio retained from before the disable, as when it subscribed a moment earlier. Disabling a rendition in `@moq/publish`, as hiding video does, keeps it in the catalog this way rather than removing it, so it is a one-field update; a disabled video rendition encodes one black keyframe first, so a player that predates the field shows black instead of a frozen picture. The field is written only when false. The older video `stalled` field is no longer written, and readers ignore it. A muted `@moq/publish` microphone is not disabled: it stays enabled and sends nothing, ending each utterance with a discontinuity marker, so listeners stay subscribed and play the gap as intentional silence. An unmuted one does the same while its talker is silent.
- **Preference.** A video rendition may carry a signed `preference`, default `0`. A player keeps only the highest preference it can decode, then picks by size and bitrate within it, as do the single-rendition egresses (RTMP play, WHEP, FLV, the transcoder's source). A publisher that adds its own H.264 copy of an H.265 source marks the copy `-1`, so only a viewer that can't decode H.265 pulls it. A player with a viewport (`@moq/watch`, `moq play`) first keeps the renditions shown in its orientation, by display size after rotation, so `preference` ranks within one: a broadcast that carries both a landscape and a portrait program ranks its main orientation higher, which a consumer without a viewport takes, and a portrait viewer switches to the portrait renditions. A square rendition suits either. Picking a rendition by name still overrides it. The field is written only when nonzero.
- **Join interval.** The root `sync.interval` promises that every audio and video track starts a group at least that often outside a pause, such as a muted microphone sending nothing: the longest GOP any of them publishes. It is fixed for the broadcast, and the [HLS gateway](/bin/hls) numbers every rendition's segments on the grid it fixes, segments the smallest whole multiple of it at least a second long, from each group's first timestamp, so renditions share segment numbers. A publisher that knows its GOP declares it; Rust publishers (`moq-mux` and everything built on it) without one measure each track over its first second or group, whichever is longer, and `@moq/publish` declares its encoders' `join`. A broadcast without it gets no HLS playlists.
- **Clock.** The optional `sync.clock` maps PTS zero to wall time so every media track shares one fixed epoch after timescale conversion. Publish independent sources as sibling broadcasts under one prefix, such as `robot/front.hang` and `robot/rear.hang`; each broadcast's audio and video entries are renditions of that source. In one Rust publisher, pass the same `moq_mux::Clock` to each `catalog::Config::with_clock`; JS publishers on one page already share the page clock. Consumers discover the siblings by prefix and align their timestamps through the advertised wall mapping, as [`<moq-sync>`](/lib/js/watch#playing-in-step) does to play several `<moq-watch>` in step.
- **Version.** A `version` is the oldest moq-hang draft that reads what carries it correctly. Each rendition, data track, and include carries its own, written only when it uses a field newer than 03 that an older reader would misread: a `track` that differs from its ID. A reader skips an entry whose `version` is above its own and plays the rest, so a new-style rendition beside a compatibility one costs a moq-hang-04 or later player only the new one (a 03 reader has no version rule: it ignores `version` and `track` and subscribes to the ID, so while 03 players matter, keep `track` equal to the ID), and the parsed catalog lists what was skipped so a player can say to upgrade. The root `version` is for a change to the root itself, which nothing in 04 makes: a reader refuses a catalog whose root `version` is above its own with a typed "unsupported catalog version" error. An additive field never raises either.
- **Extensions.** The root is a loose object. Applications add their own sections next to the ones hang defines, under a namespaced key such as `com.example.scte35`, optionally naming a track that carries the data. Every library exposes a way to write your section without clobbering the built-in ones, and readers ignore what they don't know. A section hang defines must have its shape: a `text`, `stats`, or any other built-in key holding something else refuses the whole catalog rather than reading as absent.

## Media reports

A catalog may name a publisher snapshot track with `stats: { track }` and
solicit viewer feedback with `echo: { path }`. Applications can use hang's
shared Rust and JS snapshot schemas to exchange per-rendition delivery and
playout measurements, alongside transport measurements. The echo path is
relative to the catalog broadcast; an application assigns one `.echo` name
per viewer and soliciting catalog, and controls the token rights under that
prefix. Echo broadcasts carry `feedback.json` without a media catalog, so
readers can filter them from playable content.

These are schema and discovery primitives. The publishers and players do not
automatically produce reports. MSF catalogs omit both sections. See
[media stats](/concept/stats#media-reports) for the roles and the
[hang draft](/draft/moq-hang#media-stats) for their contract.

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
      "chat": { "mode": "stream", "schema": "https://example.com/chat.schema.json" },
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

A sliding window, a bounded log a reader can join in the middle, is a framing
in [`moq-json`](/lib/rs/moq-json) rather than a catalog mode. Both ends opt
into it on a raw track.

The rest is descriptive: `schema` on a JSON track, `mime` on a binary one, `bitrate`, `jitter`, and `delay` with the same meaning as for media, plus
the optional `broadcast` reference and `track` name. A publisher measures `delay`
only from payloads stamped with their capture time on the broadcast clock. A
consumer that doesn't recognize a `mode` ignores that track and round-trips it
verbatim. Compression is not a catalog field: a JSON track is a compressed track,
and moq-net hands every reader plain frames.

The catalog entry lives as long as its producer: dropping it retracts the
track. [`moq-json`](/lib/rs/moq-json) (and its TypeScript twin) reads and writes
JSON payloads; a binary track's payloads are the frames themselves.

An application with its own per-track fields can list a data track in its own
root section instead, nesting the JSON or binary config in a `config` field
beside the application fields so future config fields cannot collide with them.
Name the section with a namespaced key such as
`com.example.mavlink`. A generic consumer only finds tracks in `json` and
`binary`.

## Transport streams

A broadcast can carry an MPEG transport stream whole instead of demultiplexed
into renditions, for what demultiplexing cannot carry, such as a scrambled
service. The `mpegts` section's `passthrough` member names the one track holding
it:

```json
{
  "mpegts": {
    "muxRate": 2499999,
    "passthrough": { "track": "0.m2ts", "randomAccess": true }
  }
}
```

Each frame is a run of whole 188-byte packets in input order, timestamped on
the multiplex's own PCR clock, which never steps back: a source whose clock
steps back is a new broadcast. `randomAccess` says whether every published group
began where a receiver can start decoding, and `muxRate` is the measured
constant multiplex rate in bits per second, absent for a variable-rate source.
A player has nothing to decode here. `moq import ts --passthrough` writes this
member (see [MPEG-TS](/bin/ts#passthrough-mode)); the section, which also
describes a demultiplexed stream, is in
[draft-lcurley-moq-mpegts](/draft/moq-mpegts).

## Container

The `container.kind` on each rendition says how frames are framed:

| Kind | Frame layout | Use |
| --- | --- | --- |
| `legacy` | varint microsecond timestamp + codec payload | The default. Carries its own timestamp on any transport. |
| `cmaf` | `moof` + `mdat` | fMP4 passthrough for HLS/DASH interop; ~100 bytes per frame. |
| `loc` | small property block + payload | The IETF [LOC](/concept/standard#loc) container. |
| `raw` | codec payload; the transport timestamp is the presentation time | No overhead, for streams without B-frames. |
| `cts` | varint composition offset (track timescale) + codec payload | Reordered video on a decode-ordered track. Decode only for now. |

`raw` and `cts` take the time from each frame's transport timestamp, so they
need a timed track whose every hop carries per-frame timestamps. moq-lite does;
a moq-transport hop gives every object its group's time until
[moq-timestamp](/draft/moq-timestamp) object timestamps land, so use `legacy`
or `loc` for viewers behind one. Publishers refuse `cts` until they can write it. A consumer skips
renditions with a kind it doesn't recognize and carries them through when
republishing the catalog.

A CMAF frame's timestamp is its fragment's earliest presentation time, and the
broadcast timeline: `tfdt` only places the samples relative to each other. A
publisher can move a passthrough track to another timeline by changing the frame
timestamp alone, without rewriting the fragment. An untimed track's frames carry
no timestamp, so their samples present at `tfdt`. A timed CMAF track declares the
init segment's `mdhd` timescale, so the frame timestamp lands on a media tick.

A Legacy, LOC, or raw video publisher can close the last frame's duration with an empty
codec payload whose timestamp is that frame's exclusive end. Consumers treat it
as metadata and never pass it to a decoder. This lets a group close immediately
without waiting for the next frame. Audio has codec-defined durations, and CMAF
carries sample durations directly, so neither needs per-group duration markers.
Audio retains its separate terminal marker before codec drain packets, allowing
consumers to discard encoder padding beyond the source endpoint. On a data
track, a group of one object with no bytes marks a discontinuity. Every other
empty payload stays data, such as an empty text cue, except under `raw`, whose
empty payload is that object: a `raw` data track can't carry one.

## Groups and keyframes

A video group is a GoP: it begins with a keyframe and holds the frames that
depend on it. That alignment is what makes MoQ's congestion behavior safe. A
relay can drop a whole group, a viewer can join at any group boundary, and the
decoder never sees a frame whose reference is missing. Audio frames are
independent, so an audio group can end at any frame; the first-party encoders
put one packet in each unless told otherwise.

The `description` field carries out-of-band codec setup (an `avcC` box for
H.264) and decides the framing. With one, H.264 and H.265 frames are
length-prefixed by its NAL length size; without one, they are Annex-B and carry
their parameter sets before each keyframe. The codec string only says where the
parameter sets live: `avc1`/`hvc1` keep them in the `description`, while
`avc3`/`hev1` may repeat them in band, with or without one. Decoders should
handle every combination. CMAF samples are always length-prefixed, so a CMAF
track always carries the configuration record, even when the parameter sets
ride in the samples.

## Your own format

hang is a convention, not a requirement. If you control both ends, publish
whatever frames you like on raw tracks; the relay never looks inside them. The
[MoQ Boy](/bin/demo) demo mixes hang media tracks with JSON status and command
tracks on the same broadcast.
