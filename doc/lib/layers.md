---
title: Layers
description: Where each layer the bindings wrap lives in every language
---

# Layers

The bindings keep Rust's layers. moq-net sits at the root of each package, and
each layer above it has its own namespace: `media`, `json`, `audio`, and
`video`. Rust's other crates, such as moq-auth and moq-room, have no
binding. A type keeps its short name in every language, so Python's
`moq.json.SnapshotProducer` is `Json.SnapshotProducer` in Swift. Find the layer
below, then look up its methods in that language's API reference.

A layer's type is built from the handle it wraps, never reached through the
broadcast: a JSON producer takes the broadcast and its track, and a codec
producer takes a `media.CatalogProducer`. A layer may reuse a net noun, and the
namespace tells them apart: `media.TrackConsumer` reads what
`media.TrackProducer` writes, a level above the raw `TrackConsumer`.

Exceptions:

- **C++** is one flat `moq::` namespace with the layer folded into the name (`moq::MediaTrackProducer`, `moq::JsonSnapshotProducer`).
- **C** is the hand-written moq-c ABI, grouped by direction (`moq_publish_*`, `moq_consume_*`) rather than by layer, so its rows name function prefixes. Its `moq_publish_video` and `moq_consume_video` move encoded frames (media); the codecs are `moq_encode_*` and `moq_decode_*`, which always publish to or subscribe from a broadcast, so C has no codec-only `Encoder`.
- **Dart** has no `audio` or `video` library, because its published binaries carry no codecs.
- **TypeScript** is a separate implementation with its own [packages](/lib/js/), so it is not mapped here.

## net

Sessions, origins, broadcasts, tracks, and groups: `Client`, `Server`,
`Session`, `OriginProducer`, `BroadcastProducer`, `TrackProducer`,
`GroupProducer`, and their consumers.

| Language | Module | Example |
| --- | --- | --- |
| Rust | [`moq_net`](/lib/rs/moq-net), with `moq_tokio` to connect and listen | `moq_net::broadcast::Producer` |
| Python | `moq` | `moq.BroadcastProducer` |
| Go | `moq.dev/moq` | `moq.BroadcastProducer` |
| Kotlin | `dev.moq` | `dev.moq.BroadcastProducer` |
| Swift | `Moq` | `BroadcastProducer` |
| Dart | `package:moq/moq.dart` | `BroadcastProducer` |
| C++ | `moq::` | `moq::BroadcastProducer` |
| C | `moq_session_*`, `moq_origin_*`, `moq_publish_track_*`, `moq_consume_track_*` | `moq_publish_track` |

## media

The catalog, encoded-media imports, and container-decoded reads, merging
Rust's hang and moq-mux: `CatalogProducer`, `CatalogConsumer`, `Timebase`,
`TrackProducer`, `TrackStreamProducer`, `ContainerProducer`, `TrackConsumer`,
and `GroupConsumer`.

| Language | Module | Example |
| --- | --- | --- |
| Rust | [`hang`](/lib/rs/hang) and [`moq_mux`](/lib/rs/moq-mux) | `moq_mux::catalog::Producer` |
| Python | `moq.media` | `moq.media.CatalogProducer` |
| Go | `moq.dev/moq/media` | `media.CatalogProducer` |
| Kotlin | `dev.moq.media` | `dev.moq.media.CatalogProducer` |
| Swift | `Media` | `Media.CatalogProducer` |
| Dart | `package:moq/media.dart` | `media.CatalogProducer` |
| C++ | `moq::Media*`, plus catalog records such as `moq::Catalog` and `moq::AudioInit` | `moq::MediaCatalogProducer` |
| C | `moq_publish_audio`, `moq_publish_video`, `moq_publish_media_*`, `moq_publish_container_*`, `moq_publish_catalog_*`, `moq_consume_audio`, `moq_consume_video`, `moq_consume_catalog_*` | `moq_publish_catalog_section` |

## json

JSON tracks as a latest value with merge-patch deltas, an append log, or a
sliding window of records: `SnapshotProducer`, `SnapshotConsumer`,
`StreamProducer`, `StreamConsumer`, `WindowProducer`, and `WindowConsumer`.
The catalog has no window mode, so a window track is not advertised there.

| Language | Module | Example |
| --- | --- | --- |
| Rust | [`moq_json`](/lib/rs/moq-json) | `moq_json::snapshot::Producer` |
| Python | `moq.json` | `moq.json.SnapshotProducer` |
| Go | `moq.dev/moq/json` | `json.SnapshotProducer` |
| Kotlin | `dev.moq.json` | `dev.moq.json.SnapshotProducer` |
| Swift | `Json` | `Json.SnapshotProducer` |
| Dart | `package:moq/json.dart` | `json.SnapshotProducer` |
| C++ | `moq::Json*` | `moq::JsonSnapshotProducer` |
| C | `moq_publish_json_*`, `moq_consume_json_*` | `moq_publish_json_snapshot` |

Go code usually imports it under an alias, next to `encoding/json`.

Compression is the track's, not this layer's: publish a JSON track with
`compress` set on its track info, and a consumer reads it like any other.
Opaque bytes go on a raw net track.

## audio

PCM encoded and decoded inside the binding: `Producer` for a published track,
`Consumer` for a decoded one, and the codec-only `Encoder`.

| Language | Module | Example |
| --- | --- | --- |
| Rust | [`moq_audio`](/lib/rs/moq-audio) | `moq_audio::encode::Producer` |
| Python | `moq.audio` | `moq.audio.Producer` |
| Go | `moq.dev/moq/audio` | `audio.Producer` |
| Kotlin | `dev.moq.audio` | `dev.moq.audio.Producer` |
| Swift | `Audio` | `Audio.Producer` |
| Dart | none | |
| C++ | `moq::Audio*` | `moq::AudioProducer` |
| C | `moq_encode_audio_*`, `moq_decode_audio_*` | `moq_encode_audio` |

## video

Raw pixels encoded and decoded inside the binding: `Producer` for a published
track, `Consumer` for a decoded one, and the codec-only `Encoder`.

| Language | Module | Example |
| --- | --- | --- |
| Rust | [`moq_video`](/lib/rs/moq-video) | `moq_video::encode::Producer` |
| Python | `moq.video` | `moq.video.Producer` |
| Go | `moq.dev/moq/video` | `video.Producer` |
| Kotlin | `dev.moq.video` | `dev.moq.video.Producer` |
| Swift | `Video` | `Video.Producer` |
| Dart | none | |
| C++ | `moq::Video*` | `moq::VideoProducer` |
| C | `moq_encode_video_*`, `moq_decode_video_*` | `moq_encode_video` |
