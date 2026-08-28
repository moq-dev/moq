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

Each audio, video, or text rendition may carry a human-readable `label` for a
track picker. The rendition map key remains the transport track name used to
subscribe, so labels do not need to be unique and changing one does not rename
the track.

Each media track is described using the [WebCodecs specification](https://www.w3.org/TR/webcodecs/) and we plan to support every codec in the [WebCodecs registry](https://w3c.github.io/webcodecs/codec_registry.html).

### Example

Here is Big Buck Bunny's `catalog.json` as of 2026-02-02:

```json
{
  "video": {
    "renditions": {
      "video0": {
        "label": "HD",
        "codec": "avc1.64001f",
        "description": "0164001fffe100196764001fac2484014016ec0440000003004000000c23c60c9201000568ee32c8b0",
        "codedWidth": 1280,
        "codedHeight": 720,
        "container": { "kind": "legacy" }
      }
    }
  },
  "audio": {
    "renditions": {
      "audio1": {
        "label": "English",
        "codec": "mp4a.40.2",
        "sampleRate": 44100,
        "numberOfChannels": 2,
        "bitrate": 283637,
        "container": { "kind": "legacy" }
      }
    }
  }
}
```

### Compression

The catalog is published on two tracks with identical content: `catalog.json` (plain JSON) and `catalog.json.z` (the same JSON, DEFLATE-compressed per group).
A publisher always serves both; a consumer reads whichever it prefers and defaults to the uncompressed `catalog.json`.

The compression is the group-scoped `deflate-raw` ([RFC 1951](https://www.rfc-editor.org/rfc/rfc1951.html)) stream used by `@moq/json` / `moq-json`, interoperable between the browser and native.
To read the compressed track, opt in explicitly: pass `--catalog-format hangz` to `moq export`, `CatalogFormat::HangZ` in Rust, or `catalogFormat: "hangz"` to `@moq/watch`.
The `.hang` broadcast suffix is unchanged: the compressed track is an extra track on the same broadcast, not a different broadcast name.

`catalog.json.z` and the `.timeline.z` tracks are always compressed and known by their role.
Everywhere else compression is declared, not guessed: a data track carries a `compression` field (see below), and the `.z` suffix on a track name is a naming convention a consumer must never read as a signal.

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

### Text

[See the latest schema](https://github.com/moq-dev/moq/blob/main/js/hang/src/catalog/text.ts).

Captions and subtitles are their own tracks, not part of the video bitstream, so the relay stays media-agnostic and a viewer only downloads the language it picked.
Text is split into renditions the same way as audio and video, typically one per language.

There is no WebCodecs decoder for text, so a consumer parses each cue itself and renders it as an overlay.
The `format` field says how: `vtt` (a self-contained WebVTT segment per frame), `ttml`, or `utf8` (raw text shown until the next cue).
The `role` field is `subtitle` (spoken dialogue) or `caption` (all audio, including non-speech sounds), and `lang` / `label` drive the track picker.

The frame timestamp carries the cue's start time on the same media clock as audio and video, so cues schedule against the same playhead with no separate pacing.
The section is omitted entirely when a broadcast publishes no captions.

A publisher can set a rendition's optional `stalled` field to recommend temporarily avoiding it without removing or closing the track.
Players prefer decoder-supported unstalled renditions and can fall back to a stalled rendition when none remain.

### Cross-broadcast renditions

A rendition may set an optional `broadcast` field: a path relative to the broadcast that served the catalog (e.g. `"./source"`), pointing at another broadcast that publishes the actual track.
A consumer resolves a non-empty reference like a relative URL: it replaces the catalog broadcast's last path segment, then applies `.` and `..` segments. An empty reference names the catalog broadcast itself.
It resolves against the path it reached the catalog broadcast at, not one the publisher declares, so a reference can only ever name a broadcast the consumer could have named itself, and subscribes to the track there over the same connection.
When the field is absent, the track lives in the same broadcast as the catalog.
A reference that walks above the root (more `..` than the catalog path has segments) names no broadcast, so the whole catalog is rejected rather than pointed at whatever path the walk stops on.
The root is the consumer's authorized subtree, so such a reference is an attempt to name content it cannot reach: a publisher emitting one has a bug, and quietly serving the remaining renditions would hide that.

This lets a transcoder publish a sidecar catalog that adds new renditions while pointing unchanged ones at the original broadcast, instead of re-publishing those bytes through the transcoder.
For example, a transcoder consuming `room/source` can publish `room/transcode` whose catalog contains a downscaled `480p` rendition plus the original `1080p` rendition marked `"broadcast": "./source"`.
A viewer of `room/transcode` then pulls `480p` from the transcoder and `1080p` directly from the source, and the relay dedupes the source subscription with the transcoder's own.

Rejection happens where the catalog is read, not where a track is subscribed: the rendition set drives track layouts, playlists, codec lists, and quality selectors, so a reference caught any later would already have been offered and chosen. In Rust the `moq-mux` catalog stream rejects it and every exporter reads through that stream; `@moq/watch` rejects the catalog it would otherwise publish.

Publishers author the reference with the inverse operation, so the two stay in step: `Path::relative` in Rust (`moq-net`) and `Path.relative` in TypeScript (`@moq/net`), each taking the target broadcast and the catalog broadcast it is named from.
Both refuse a target no reference can name, since a path segment may itself be called `.` or `..`.

`@moq/watch` resolves the reference automatically. In Rust, the `moq-mux` exporters do the same: they take a `Source::new(origin, path)`, and both the catalog broadcast and any referenced broadcast resolve through the origin over the same connection.

### Data tracks

Not everything in a broadcast is media. A chat log, a telemetry feed, a thumbnail, a serialized game state: the catalog lists these in two sections alongside `video` and `audio`, split by what a generic consumer can do with the payload.

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

A `json` track's frames are UTF-8 JSON values, so a relay, a debugger, or an archiver can parse and re-serialize them without knowing the application. A `binary` track's frames are opaque bytes, which only the application can interpret. Each map is keyed by track name, and unlike the media sections these are not rendition sets: the entries are distinct tracks, not alternatives to choose between. A section is omitted entirely when it holds no tracks.

`mode` is required and says how a track's groups compose its frames. There is no default, because reading an append log as a latest-value document silently discards every payload but the last:

- `snapshot` is lossy. Each group is self-contained and supersedes the previous one, so a consumer reads only the newest and a publisher may drop older ones. A JSON track may follow a group's first frame with merge-patch deltas; a binary track writes one frame per group.
- `stream` is lossless in the sense that nothing supersedes anything else. One payload per frame, in order, all in a single group that is never rolled. A publisher that cannot write a payload closes the track, since a second group would present a gap as if it were a complete log. A consumer reads the one group the track carries and fails the read if a second appears, rather than yielding the remainder as though the log were continuous. Retention is still bounded by the group cache: a log longer than the cache holds evicts its earliest frames, and a consumer that falls behind or joins late then cannot read the log at all, since the read fails when it reaches the evicted prefix rather than silently resuming partway through. That is deliberate, because a partial log presented as a whole one is what this mode exists to prevent, and under compression the retained frames are undecodable anyway without the evicted prefix as context. Keep a stream track's log inside what its groups retain, and split anything unbounded across successive tracks.

`compression` names the compression applied to the frames, or is absent when they are uncompressed. `deflate` is the same group-scoped `deflate-raw` the catalog uses. The remaining fields are optional and descriptive: `schema` (a JSON Schema URL) on a JSON track, `mime` on a binary one. Both kinds also accept the `broadcast` and `timeline` fields a media rendition takes.

A consumer that does not recognize a track's `mode` or `compression` must ignore that track. It still round-trips verbatim, so a relay that reparses and republishes the catalog never corrupts a track it cannot read.

#### Publishing and reading

In Rust you create the track on the broadcast, as you would for a media track, and hand it to the catalog. The catalog writes the entry and drops it when the handle drops, so a track is never advertised without a publisher behind it. The catalog key is the track's own name, with no `.z` suffix even when compressed, since the entry's compression flag is what a consumer reads:

```rust
let track = broadcast.create_track("chat", None)?;
let mut chat = catalog.json_stream::<Message>(track, json::Config::default().with_compression(true))?;
chat.append(&message)?;

let track = broadcast.create_track("thumbnail", None)?;
let mut thumbnail = catalog.binary_snapshot(track, binary::Config::default().with_mime("image/jpeg"))?;
thumbnail.update(jpeg)?;
```

There is a producer type per mode (`json_snapshot` / `json_stream`, `binary_snapshot` / `binary_stream`) so `append` on a latest-value track does not compile.

The read side names the track once. `catalog.json_track(name)` returns an entry pairing the name with its config, and the entry subscribes itself:

```rust
let entry = catalog.json_track("chat").expect("no chat track");
let mut chat = entry.subscribe::<Message>(&source).await?;
while let Some(message) = chat.next().await? {
    // ...
}
```

The entry supplies the track's mode and compression, so a reader cannot pair the wrong ones with the track, and it resolves through a `Source`, so an entry pointing at a sibling broadcast is followed rather than read from the wrong place.

There is one consumer type rather than one per mode. Both modes hand the caller the same thing, a sequence of values ending when the track does, so a reader writes one loop either way; what differs is loss semantics, and `consumer.mode()` answers that for the rare reader that can only work with one. Discovery is the same entries: `catalog.json_tracks()` and `catalog.binary_tracks()` enumerate them, since the catalog is the only thing that announces a data track.

In the browser the pieces are the same, assembled by hand: read the entry from `catalog.json.tracks` (or `catalog.binary.tracks`), subscribe to the track by name, and hand it to `@moq/json` or `@moq/binary`. Two mappings to do yourself, since the packages take the mode and compression as code rather than as catalog values:

```ts
const entry = catalog.json?.tracks.chat;
if (!entry || !Catalog.modeSupported(entry.mode)) return;
if (entry.compression !== undefined && !Catalog.compressionSupported(entry.compression)) return;

const compression = entry.compression === "deflate";

// Honor a cross-broadcast reference the same way the Rust `Entry::subscribe` does; without this
// you would read an unrelated same-named track in the catalog's own broadcast. A catalog is
// untrusted input, so use `tryResolve`: it returns undefined when the reference escapes above the
// root, which the spec says to ignore rather than clamp onto some other valid broadcast.
let source = broadcast;
if (entry.broadcast !== undefined) {
    const path = Path.tryResolve(catalogPath, entry.broadcast);
    if (!path) return;
    source = await connection.consume(path);
}
const track = source.subscribe("chat");
const consumer =
    entry.mode === "stream" ? new Json.Stream.Consumer(track, { compression }) : new Json.Snapshot.Consumer(track, { compression });
```

`mode` picks the namespace (`Stream` or `Snapshot`), and `compression` is a boolean there while the catalog carries `"deflate"` or nothing. Check both against `modeSupported` / `compressionSupported` first and skip the track if either is unrecognized, rather than guessing.

### Extensions

The base catalog carries the media sections (`video` and `audio`) and the data track sections (`json` and `binary`).
Applications add their own root sections (for example `scte35`) without modifying hang.

The catalog is a JSON document published through the merge-patch snapshot helper (the `Snapshot` mode of `@moq/json` / `moq-json`), and an extension is just an extra top-level key:

- **Reading**: the base schema is permissive, so unknown sections pass through validation untouched.
  A base consumer ignores them; an extension reads its own section and treats its absence as "not present".
  In TypeScript, build an extended schema with `z.extend(Catalog.RootSchema, { scte35: ... })`.
  In Rust, either flatten the catalog into your own struct with `#[serde(flatten)]` for typed access, or read sections untyped from an `Extra` catalog, which keeps unknown keys as raw JSON (`catalog.section("scte35")`). The `()` default drops sections it doesn't model.
  The FFI bindings always use the untyped form, one JSON string per section keyed by name (`catalog.sections["scte35"]` in Python, `moq_consume_catalog_section()` / `moq_consume_catalog_section_at()` in C).
- **Writing**: the catalog producer holds one shared document.
  Each owner edits only its own keys and publishes: `producer.mutate(c => { c.scte35 = ... })` in TypeScript; the `Deref`/`DerefMut` lock guard from `producer.lock()` for a typed Rust extension, or `producer.set_section("scte35", value)` for an untyped one; `broadcast.set_catalog_section("scte35", value)` in Python; `moq_publish_catalog_section()` in C.
  Every edit starts from the latest value, so the base media sections and any extension sections compose instead of clobbering one another.
  Removing a key publishes a deletion (`producer.remove_section(...)`, `broadcast.remove_catalog_section(...)` in Python, `moq_publish_catalog_section_remove()` in C), which a consumer reads as the section being removed.

This keeps application-specific sections in the application layer while the base catalog stays generic.

### Custom tracks

Reach for the `json` and `binary` sections above first: they name a track and say how to read it, so a generic consumer can find and decode one without application support.

An application still needs its own catalog section when the metadata is not a track at all (a low-rate value carried inline in the catalog) or when it has to say something the data sections do not model. Such a section can reference a separate track in the same broadcast, which the relay treats like any other; only the publisher and consumer give it meaning.

The `@moq/publish` and `@moq/watch` components publish and subscribe to these tracks generically, with no per-application support. Each exposes a low-level track hook, and the application uses `@moq/json` to encode the payload itself:

- **Publish**: `broadcast.publishTrack(name, serve)` runs `serve(track, effect)` per subscriber. For JSON, serve each track from a shared track-less `Json.Snapshot.Producer` (the same fan-out producer the catalog uses, seeding late joiners with the latest value). Advertise the track by writing your own catalog section with `broadcast.catalog.mutate(...)`.
- **Watch**: `broadcast.subscribeTrack(name, priority, consume)` follows the active broadcast across reconnects. For JSON, wrap the track in a `Json.Snapshot.Consumer` inside `consume`. Read your section back from `broadcast.catalog` (unknown sections pass through the loose schema).

So an application supports something like SCTE-35 entirely in its own code: publish an `scte35` section (and optionally a track) on one side, read it on the other, without hang, `@moq/publish`, or `@moq/watch` knowing anything about SCTE-35.

## Container

The catalog also contains a `container` field for each rendition used to denote the encoding of each track.
Unfortunately, the raw codec bitstream lacks timestamp information so we need some sort of container.

Containers can support additional features and configuration.
For example, `CMAF` specifies a timescale instead of hard-coding it to microseconds like `legacy`.

The `kind` field selects the framing and new kinds can be added over time.
A consumer ignores any rendition whose `kind` it doesn't recognize, keeping the rest of the catalog usable, and carries the unrecognized entry through untouched when it republishes the catalog.

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
