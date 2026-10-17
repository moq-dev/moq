# Migrating moq-ffi

How to update your code for each breaking change, newest first.
These are the UniFFI names; each language wrapper has its own `MIGRATE.md` in its own casing.

## Unreleased

### Receive estimates come from `MoqSession::recv_bandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `MoqSession::recv_bandwidth` returns to request PROBE, read it with `peek()` or wait with `changed()`, and drop it to stop. `MoqConnectionStats::estimated_recv_rate_bps` is gone.

```rust
let bandwidth = session.recv_bandwidth();
let rate = bandwidth.as_ref().and_then(|b| b.peek());
```

### `available()` waits for the next publisher instance (#5700)

`MoqAnnouncedBroadcast::available` returns the broadcast serving the path on its first call, as before.
Each later call waits for the next instance to take over the path, instead of returning whatever serves it now.

### A timed track refuses a group that rewinds (#5803)

`write_frame` on a `MoqTrackProducer` or `MoqGroupProducer` fails with a `MoqError::Protocol` of kind `TimestampMismatch` when a group's first frame starts before a lower-sequence group's, or after a higher one's.
A publisher whose timeline restarts publishes a new broadcast instead.

### Bandwidth reserves a track's demand (#5845)

`MoqBandwidth::reserve` takes a `MoqTrackDemand` instead of a `MoqTrackProducer`, so raw, media, and JSON tracks reserve the same way, and it no longer fails: pass `track.demand()?`.

### Media subscribe and fetch take a catalog rendition (#5834)

`MoqMediaTrackConfig` and `MoqMediaGroupConfig` replace `name` and `container` with a `rendition` (`MoqRendition::Video` or `Audio`: the catalog `id` and its config) and follow its `broadcast` and `track` themselves, so drop the `resolve` call and the `track`-or-ID fallback.
The audio and video decode consumers' `name` argument is `id`.

```rust
let config = MoqMediaTrackConfig { rendition: MoqRendition::Video { id, config: video }, subscription: None };
```

### Native codecs have audio and video namespaces (#5694)

`MoqBroadcastProducer::encode_audio` / `encode_video` are the `MoqAudioProducer::new` / `MoqVideoProducer::new` constructors, which take the broadcast's `MoqMediaCatalogProducer`.
The audio track name moves into `MoqAudioEncoderOutput::track`, and a zero `frame_duration_us` (the new default) takes the codec's own frame.
`MoqBroadcastConsumer::decode_audio` / `decode_video` are `MoqAudioConsumer::subscribe` / `MoqVideoConsumer::subscribe`, taking the broadcast first.
The producers' `name()`, `used()`, and `unused()` are gone; read subscriber state through `demand()`.
`MoqVideoEncoderOutput::gop` is `Option<MoqVideoGop>`, so `Some(30)` is `Some(MoqVideoGop::Keyframe { interval: 30 })`.
Codec-only `MoqAudioEncoder` / `MoqVideoEncoder` encode without publishing.

```rust
let catalog = MoqMediaCatalogProducer::new(broadcast)?;
let video = MoqVideoProducer::new(&catalog, input, output, None)?;
let frames = MoqVideoConsumer::subscribe(&consumer, name, rendition, decoder).await?;
```

### Background panics are reported (#5562)

A panic in a binding-owned background task reports `MoqError::Task { reason }` with the panic message, and other objects keep running.
Handle the new case where you match on errors.

### Announcements are flat events with a restart (#4266, #4384, #5087)

`MoqAnnounceConsumer::next` returns `Option<MoqAnnounceEvent>`: `Start`, `Update`, `End`, or `Restart`, each carrying a `MoqAnnounce` record (`prefix`, `captures`, `route`).
The `MoqAnnounceUpdate` object and its `active()` are gone.
`Restart` means another publisher instance now serves the prefix: request the path again.
Subscribe on `Start` or `Restart` where you checked `active()`.

### "Epoch" means only the publisher epoch (#5146)

`MoqSession::epoch()` is `connects()`, and `MoqAudioProducer::reset_epoch()` is `reanchor()`.

### Clients and servers take a config record (#4519)

`MoqClient::new()` and `MoqServer::new()` plus their setters are `MoqClient::new(MoqClientConfig)` and `MoqServer::new(MoqServerConfig)`, with nested `tls`, `quic`, `websocket`, and `backoff` records and a `versions` list.
A value the native side cannot use fails construction with `MoqError::Config`, including a bad bind address that used to fail later as `Bind`.
The `subscribe` origin is `consume`.
A server request's `set_publish` / `set_consume` are arguments to `accept(publish, consume)`, where null inherits the server's origin.

```rust
let client = MoqClient::new(MoqClientConfig {
	tls: MoqClientTls { insecure: true, ..Default::default() },
	consume: Some(origin),
	..Default::default()
})?;
```

### Data tracks wrap a track (#4519)

The broadcast's `publish_binary_*`, `publish_json_*`, and `subscribe_json_*` are gone.
Create the track with `publish_track` or `subscribe_track`, then wrap it.

```rust
let track = broadcast.publish_track("state".into(), None)?;
let producer = MoqJsonSnapshotProducer::new(&broadcast, &track, config)?;
```

Likewise `MoqJsonStreamProducer`, `MoqFlateSnapshotProducer` / `MoqFlateStreamProducer` (taking `MoqFlateConfig`), and `MoqJsonSnapshotConsumer` / `MoqJsonStreamConsumer` built from a `MoqTrackConsumer`.

### Media lives in a `media` namespace (#4519)

The broadcast's `publish_audio`, `publish_video`, `publish_container`, their `_on_track` and `_stream` variants, `set_video_properties`, `set_catalog_section`, `subscribe_catalog`, `subscribe_media`, and `fetch_media_group` are gone.
Construct `MoqMediaTrackProducer::audio` / `video` from the broadcast, a `MoqMediaTarget` (`Named` or `Requested`), and the init record; or `MoqMediaContainerProducer`, `MoqMediaCatalogProducer`, `MoqMediaCatalogConsumer::subscribe`, `MoqMediaTrackConsumer::subscribe`, or `MoqMediaGroupConsumer::fetch`.
Producers drop `name`, `used`, and `unused`; read them through `demand()`.

```rust
let audio = MoqMediaTrackProducer::audio(&broadcast, MoqMediaTarget::Named { name: None }, init)?;
```

### Container byte-stream importers take an init record

`MoqMediaContainerStreamProducer` takes a `MoqContainerStreamInit` holding the format instead of a bare format.
Its optional `timebase`, also on `MoqContainerInit`, shares one source's offset across container imports; leave it unset for independent offsets.

### Error payloads are named

`MoqError` cases that carried a positional string (`Transport`, `Internal`, `Connect`, ...) name it `reason`, so match `MoqError::Transport { reason }`.
The Display text is unchanged.

### Timestamps are optional

`MoqFrame.timestamp_us` and `MoqDatagram.timestamp_us` are optional, null on a frame read from an untimed track, and default to null.
`MoqTrackInfo.timescale` is null on a received untimed track, and a null timescale on publish makes the track untimed.
A raw track you publish is timed by default, so a raw write without a timestamp fails with `TimestampMismatch` instead of going out at 0.
Data operations take an optional timestamp in microseconds, and whole-frame media writes require one.
`MoqMediaCatalogProducer::now()` samples the broadcast clock for local-event stamps.
Wrap raw frame timestamps in `Some`.

### Subscriber staleness is `max_delay_us`

`MoqSubscription`, `MoqAudioDecoderOutput`, and `MoqVideoDecoderOutput` take `max_delay_us` instead of `max_age_us`.
`MoqTrackInfo.max_age_us` keeps its name: it is the publisher's retention.

### `MoqTransport` gains `WebTransport`

A WebTransport session reports `MoqTransport::WebTransport`; `Quic` now means raw QUIC only.
An exhaustive match needs one more arm, and a `Quic` check that meant any QUIC session also matches `WebTransport`.

### `stalled` is `enabled` (#4915)

`MoqVideo.stalled` is `enabled`, defaulting to true with the meaning inverted, and `MoqAudio` gains `enabled`.

### Routes have one cost (#4661)

`MoqRoute.cold` is removed; drop it.

### Decoded video frames own their surface

`MoqVideoConsumer::next` returns an `Arc<MoqVideoDecodedFrame>` object with `timestamp_us()`, `width()`, `height()`, `pixels(format)`, and `surface()`.
`MoqVideoDecoderOutput.format` is gone; set `surface: true` to keep the decoder's native surface, readable as a `MoqVideoSurface`.
Read RGBA with `pixels(MoqVideoPixelFormat::Rgba)` instead of `data`.

### Broadcast `finish` is removed (#4152)

`MoqBroadcastProducer::finish()` is gone; close the broadcast instead.

### The `android-logcat` feature is removed (#4608)

Android builds always log to logcat; drop the feature.

## 0.3 -> 0.4

### Publish split by kind

`publish_media*` is `publish_audio`, `publish_video`, or `publish_container`, each taking its own init (`MoqAudioInit` with a `MoqAudioFormat` for audio).
The raw encoder paths that used to be `publish_audio` / `publish_video` are `encode_audio` / `encode_video`.

### Renames

| Before | After |
| --- | --- |
| durations in milliseconds | microseconds (`max_age_us`, `MoqBackoff.initial_us`) |
| `estimated_send_bitrate` / `estimated_recv_bitrate` | `estimated_send_rate` / `estimated_recv_rate` |
| `set_tls_disable_verify(bool)` | `set_tls_verify(bool)` |
| `MoqAnnounced` | `MoqAnnounceConsumer` |
| `MoqAnnouncement` | `MoqAnnounceUpdate` |
| `MoqBroadcastRequest::abort` | `reject` |
| `MoqOriginOptions` | `MoqOriginConfig` |
| `MoqAudioCodec` enum | an `opus()` object (#3671) |

### Setters are fallible (#3642)

Client and server setters return an error (`Busy` during connect or listen) instead of dropping the value, so handle it.

### Protocol errors are structured

`MoqError::Protocol` carries a `MoqProtocolError` (scope, wire code, kind) in its `details` field instead of a flattened message.

### Track and group `finish()` keep the handle

`finish()` keeps the handle open, so a later `abort()` can still run.
