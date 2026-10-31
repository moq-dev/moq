# Migrating moq-ffi

How to update your code for each breaking change, newest first.
These are the UniFFI names; each language wrapper has its own `MIGRATE.md` in its own casing.

## Unreleased

### Compression moves to the track

The `MoqFlate*` producers and `MoqFlateConfig` are deleted: publish opaque bytes on a `MoqTrackProducer`.
`MoqJsonSnapshotConfig` and `MoqJsonWindowConfig` lose `compression`, and `MoqJsonStreamConfig` is removed: set `compress` on the `MoqTrackInfo` you publish the track with instead.
`MoqJsonStreamProducer::new` drops its config argument, and `MoqJsonSnapshotConsumer::new`, `MoqJsonStreamConsumer::new`, and `MoqJsonWindowConsumer::new` take only the track, reading a compressed track like a plain one.

### Paths with an empty segment are refused ([#6648](https://github.com/moq-dev/moq/pull/6648))

Every method taking a path, such as `MoqOriginProducer::create_broadcast` and `MoqOriginConsumer::request_broadcast`, returns an error for a path with a leading, trailing, or doubled `/` instead of collapsing it. Pass the canonical path, such as `room/cam`.

### Request handlers are `requests()` ([#6752](https://github.com/moq-dev/moq/pull/6752))

Rename `dynamic()` on `MoqOriginProducer`, `MoqBroadcastProducer`, `MoqTrackProducer`, and `MoqTrackRequest` to `requests()`.
Their handles `MoqOriginDynamic`, `MoqBroadcastDynamic`, and `MoqTrackDynamic` are `MoqOriginRequests`, `MoqBroadcastRequests`, and `MoqTrackRequests`; their methods are unchanged.

### `MoqAllocator` ([#6751](https://github.com/moq-dev/moq/pull/6751))

`MoqBandwidth` is `MoqAllocator`, and `MoqSession::bandwidth()` is `allocator()`, matching `moq_net::bandwidth::Allocator`. `MoqReservation::grant()` is `rate()`, matching `moq_net::bandwidth::Reservation::rate()`. The `bandwidth` option field on audio and video producers keeps its name.

### An idle reservation reads the share it would get

`MoqReservation::rate` reports the slice the track would get back while nothing subscribes to it, instead of `None`; it still claims nothing from the other tracks.
A sender that relied on `None` to hold its rate while unwatched should check the track's demand instead.

### Decoders read gaps and ends

`MoqAudioConsumer::next` returns `MoqAudioRead` and `MoqVideoConsumer::next` returns `MoqVideoRead`, mirroring `moq_audio::decode::Read` and `moq_video::decode::Read`, instead of a bare frame. Take the frame from the `Frame { frame }` variant where you used the return value before. `Gap { timestamp_us, duration }` marks lost media and `End { end_us }` where the publisher stopped its encoder; what follows either does not continue what came before, so restart playback there, or skip them to keep the old behavior. `None` still means the track ended.

### `MoqClientConfig` gains `tokens`

It defaults to empty in every generated binding; a Rust struct literal adds `tokens: vec![]` or ends with `..Default::default()`.

### Durations are typed

Every duration is a UniFFI `Duration` instead of an integer microsecond field, and drops its `_us` suffix: each language receives its own duration type (`timedelta`, `TimeInterval`, `java.time.Duration`, `time.Duration`, Dart `Duration`, C++ `std::chrono` nanoseconds).
The renamed fields are `MoqClientConfig.{timeout, race, resolution_delay}`, `MoqQuicConfig.{idle_timeout, keep_alive}`, `MoqWebSocketConfig.delay`, `MoqBackoff.{initial, max, timeout}`, `MoqConnectionStats.rtt`, `MoqGrant.expires`, `MoqTrackInfo.max_age`, `MoqSubscription.max_delay`, `MoqFetchGroupOptions.max_delay`, `MoqAudio.jitter`, `MoqAudioDecoderOutput.{max_delay, delay}`, and `MoqVideoDecoderOutput.max_delay`; the methods are `MoqFill::max_delay()` and `MoqAudioConsumer::delay()`.
`MoqAudioEncoderOutput.frame_duration_us: u32` is `frame_duration: Option<Duration>`: pass null where you passed 0 for the codec's own frame.
`MoqSubscription.max_delay` has no default, since UniFFI can't default a duration to 0: Kotlin and Swift must name it, `0` for moq-net's default or null for no subscriber limit.
Dart, Go, and C can still omit it (Dart's optional parameter, Go's nil `*time.Duration`, a zeroed C struct), which means no subscriber limit; name it there too.
Start from `moq_subscription_defaults()` where you relied on that default; a null subscription argument still means it.
`MoqTrackConsumer::info()` no longer fails.
A duration handed out saturates at `i64::MAX` nanoseconds (about 292 years), so C, C++, and Go hold every value.

### Connection state

`MoqClient::connect` returns a connecting session without waiting for its first dial to succeed, natively and in the browser. Read `MoqSession::state()` and pass its `revision` to `state_changed(after)` to observe first-dial failures and reconnects. `MoqConnectionState` carries `phase`, `connects`, `failure`, and `version` together. These replace `status()`, `connects()`, and both `failure()` polls. Cancel the returned session to stop dialing; cancelling or releasing the client does not close it.

### Exact encoder rates and publication options

Encoder configuration is breaking: `MoqVideoEncoderInput.framerate` is now a
`MoqVideoFrameRate { numerator, denominator }` (use `{30, 1}` for 30 fps or
`{30000, 1001}` for NTSC). Zero components and rates above 1,000,000 fps are
refused. `track` moves out of both encoder output records into
`MoqAudioProducerOptions` / `MoqVideoProducerOptions`, alongside `input`,
`output`, and optional `bandwidth`. Producers now take `(catalog, options)`;
codec-only encoders keep their input/output arguments and take no publication
settings. Regenerate bindings and rebuild the native library together.

### Video rendition preference

`MoqVideo` gains `preference: i32`. Set it to `0` in record literals to keep the default ranking; higher values are preferred and compatibility transcodes use `-1`. Regenerate bindings and rebuild them with the matching native library.

### `announce` takes a `MoqAnnounceRoute`

`MoqBroadcastProducer::announce` takes `MoqAnnounceRoute { cost, epoch }` instead of a `MoqRoute`: the origin builds the hop chain, so a publisher no longer passes `hops` or `anonymous`.
`MoqRoute` is only what `MoqAnnounceConsumer` reports, so its fields lose their defaults, and `MoqError::InvalidRoute` is gone.
To re-announce an observed route, copy its `cost` and `epoch`.

### A broadcast reference waits for its sibling's announcement (#6254)

Resolving a rendition's `broadcast` reference, through `MoqBroadcastConsumer::resolve` and the rendition-based subscribes and fetches, now waits for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, bound the call yourself: cancel it or race it against a timeout.

### Session shutdown drains

`MoqSession::shutdown()` still returns `()` at once, but it now requests a draining close instead of cancelling with code 0.
Await `closed()` to wait for the drain and receive delivery failures, or call `cancel(0)` to close at once, as `shutdown()` did.
If you adopted the release line's async `drain()`, call `shutdown()` and then await `closed()` instead.

### Media track naming moves out of init

`MoqAudioInit::track` and `MoqVideoInit::track` are removed.
Pass that name through `MoqMediaTarget::Named { name }` when constructing `MoqMediaTrackProducer::audio` or `video`; init records describe the codec and media only.

### Broadcast `finish` is removed

`MoqBroadcastProducer::finish()`, a deprecated alias for `close()`, is removed.
Call `MoqBroadcastProducer::close()` where you called `finish()`.
To end the content first, call `MoqMediaCatalogProducer::finish()`; the broadcast stays up until `close()`.

### JSON snapshot `delta_ratio` is `deltas`

`MoqJsonSnapshotConfig::delta_ratio` is replaced by `deltas: bool` (default `true`).
Set `deltas: false` where you set `delta_ratio: 0`.
Snapshot groups roll on joiner demand, which the producer answers on its own: publish the track with `MoqTrackInfo::refresh` (`Some(8)` matches the old default), or its groups roll only at their caps.

### Priorities are signed (#6142)

`MoqTrackInfo.priority`, `MoqSubscription.priority`, `MoqFetchGroupOptions.priority`, and `MoqGroupRequest.priority()` are `i8`, higher first, and 0 is the unset midpoint.
`MoqTrackInfo.priority` defaulted to 127 and now defaults to 0.
To keep a value's moq-lite byte, subtract 128 from it: 255 becomes 127, 128 becomes 0, and 0 becomes -128.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `announced` or `announced_broadcast`, instead of under its whole scope at connect.
`MoqOriginConsumer::request_broadcast` watches nothing, so on a client it fails with `MoqError::Unroutable` for a path nothing watches, even when the relay announces it.
Resolve the path with `announced_broadcast`, or hold `announced` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.

### `MoqContainerFormat::Fmp4` unwraps to Legacy (#6250)

A container import with `MoqContainerFormat::Fmp4` now publishes one Legacy frame per sample instead of one CMAF frame per fragment.
Pass the new `MoqContainerFormat::Cmaf` to keep publishing CMAF.

### Catalog references carry their epoch

`MoqVideo` and `MoqAudio` gain `epoch`, the publisher instance their `broadcast` reference names. It defaults to null in every generated binding; a Rust struct literal adds `epoch: None`.
`MoqBroadcastConsumer::resolve` takes the catalog `MoqRendition` instead of its `reference`, and reads the reference and its `epoch` from it.
Resolving, and the rendition-based consumers, fail a rendition whose broadcast is proven to serve another epoch with `Mux`.

### Timestamps stop at 2^61 - 1 microseconds (#6168)

Every `timestamp_us` input, such as `MoqTrackProducer::write_frame` or `MoqAnchor::Time`, fails with `MoqError::TimeOverflow` past `2^61 - 1` instead of `2^62 - 1`, the most moq-lite-05/06 can carry.
A received timestamp past it fails the group as a malformed track.
Clamp timestamps you build from unbounded input to `2^61 - 1`.

### Finished tracks stay until the broadcast ends (#6157)

A finished track, raw or media, now stays listed, with its catalog rendition and demand, until its broadcast closes, instead of unlisting when its producer finishes or is released.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.

### A claim answers by publishing (#5678)

`MoqOriginProducer::dynamic(prefix)` takes no `MoqRoute`, and `MoqOriginDynamic::update` is gone.
`MoqBroadcastRequest::accept` is replaced by `prepare(epoch)`, which returns a `MoqBroadcastPrepared`: fill its `broadcast()`, then `publish()` announces it at the exact path under that epoch and returns its producer.
Mint a fresh epoch per output run with `moq_mint_epoch()`. Dropping a prepared answer unpublished refuses the request.

### `MoqTrackInfo` gains `compress` (#5928)

It defaults to `false` in every generated binding; a Rust struct literal adds `compress: false`.

### Anchors can name a time (#6074)

`MoqAnchor` gains `Time { timestamp_us }`, the group containing that presentation time, so exhaustive matches need an arm for it.
`MoqTrackConsumer::update` now returns `Result<(), MoqError>`, failing with `TimestampMismatch` on a time anchor an untimed track can't serve.

### JSON consumers return `MoqJsonValue` (#5677)

`MoqJsonSnapshotConsumer::next` and `MoqJsonStreamConsumer::next` return `MoqJsonValue { value, timestamp_us }` instead of a string; `timestamp_us` is null when untimed.
Snapshot `next` now yields every state in order; call the new `MoqJsonSnapshotConsumer::latest` for the old collapse-to-newest read.
Timestamps are rounded down to a whole microsecond.

### Ranges filter reads (#5929)

A subscription now reads only the groups its own `ranges` cover, rather than starting at the lowest one and taking whatever arrives.

### Track demand reports the wanted group (#5930)

`MoqTrackDemand::is_used`, `used`, and `unused` are gone.
`wanted()` returns null while nobody subscribes, otherwise the group subscribers want next, and `wanted_changed()` waits for it to change (the first call returns at once).
Loop on `wanted_changed()` until it returns non-null where you awaited `used()`, and null where you awaited `unused()`.
`MoqTrackInfo` gains `refresh`, the ratio past which a joining subscriber asks for a new group.

### `MoqCongestionControl` gained `None` (#5926)

`MoqCongestionControl::None` runs no congestion control and no pacing, for tests and controlled links.
An exhaustive match needs one more arm.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Set `MoqQuicConfig::keylog` to `true` as well to capture them; without `SSLKEYLOGFILE` `MoqClient::new` and `MoqServer::new` fail.

### OpenH264 is a build feature (#5771)

The published libraries still carry the software H.264 fallback.
A source build (`cargo build -p moq-ffi`) leaves it out; add `--features openh264` to keep it.

### Subscriptions carry group ranges (#5732)

`MoqSubscription::group_start` and `group_end` are replaced by `ranges`, a list of `MoqRange { start, end }` whose ends are `MoqAnchor::Absolute { group, frame }` or `MoqAnchor::Relative { groups }`, and an optional `order`.
A start group `n` is `MoqRange { start: MoqAnchor::Absolute { group: n, frame: 0 }, end: None }`.

### Receive estimates come from `MoqSession::recv_bandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `MoqSession::recv_bandwidth` returns to request PROBE, read it with `rate()` or wait with `rate_changed()`, and drop it to stop. `MoqConnectionStats::estimated_recv_rate_bps` is gone.

```rust
let bandwidth = session.recv_bandwidth();
let rate = bandwidth.as_ref().and_then(|b| b.rate());
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
The audio track name belongs to `MoqAudioProducerOptions::track`, and a zero `frame_duration_us` (the new default) takes the codec's own frame.
`MoqBroadcastConsumer::decode_audio` / `decode_video` are `MoqAudioConsumer::subscribe` / `MoqVideoConsumer::subscribe`, taking the broadcast first.
The producers' `name()`, `used()`, and `unused()` are gone; read subscriber state through `demand()`.
`MoqVideoEncoderOutput::gop` is `Option<MoqVideoGop>`, so `Some(30)` is `Some(MoqVideoGop::Keyframe { interval: 30 })`.
Codec-only `MoqAudioEncoder` / `MoqVideoEncoder` encode without publishing.

```rust
let catalog = MoqMediaCatalogProducer::new(broadcast)?;
let video = MoqVideoProducer::new(&catalog, MoqVideoProducerOptions { input, output, track: None, bandwidth: None })?;
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

### Container byte streams use the container producer (#5919)

`MoqContainerStreamProducer` (from `publish_container_stream`) has no replacement type: a container recovers its own framing, so `MoqMediaContainerProducer` takes chunks split anywhere.
Construct it with empty `data` and write the stream through it.
`MoqContainerInit`'s optional `timebase` shares one source's offset across container imports; leave it unset for independent offsets.

```rust
let stream = MoqMediaContainerProducer::new(&broadcast, MoqContainerInit { format, data: Vec::new(), timebase: None })?;
```

### Error payloads are named

`MoqError` cases that carried a positional string (`Transport`, `Internal`, `Connect`, ...) name it `reason`, so match `MoqError::Transport { reason }`.
The Display text is unchanged.

### Timestamps are optional

`MoqFrame.timestamp_us` and `MoqDatagram.timestamp_us` are optional, null on a frame read from an untimed track, and default to null.
`MoqTrackInfo.timescale` is null on a received untimed track, and a null timescale on publish makes the track untimed.
A raw track you publish is now untimed by default: a null `timescale` no longer selects microseconds.
Set `timescale: Some(1_000_000)` to keep the released default, and provide a timestamp on each timed write.
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
