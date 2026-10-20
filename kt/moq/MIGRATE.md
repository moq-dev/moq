# Migrating the Kotlin dev.moq:moq package

How to update your code for each breaking change, newest first.

## Unreleased

### Ranges filter reads, and anchors can name a time (#5929)

A subscription now reads only the groups its own `ranges` cover.
`Anchor` gains `AnchorTime`, so exhaustive `when` expressions need a branch for it.
`TrackConsumer.update` now throws `MoqException` on a time anchor an untimed track can't serve.

### Track demand reports the wanted group (#5930)

`TrackDemand.isUsed()`, `used()`, and `unused()` are gone.
`wanted()` returns null while nobody subscribes, otherwise the group subscribers want next, and `wantedChanged()` waits for it to change (the first call returns at once).
Loop on `wantedChanged()` until it returns non-null where you called `used()`, and null where you called `unused()`.
`TrackInfo` gains `refresh`. `GroupDemand` is unchanged.

### `CongestionControl` gained `NONE` (#5926)

`CongestionControl.NONE` runs no congestion control and no pacing, for tests and controlled links.
An exhaustive `when` needs one more branch.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Set `QuicConfig.keylog` to `true` as well to capture them; without `SSLKEYLOGFILE` the client or server refuses to build.

### Subscriptions carry group ranges (#5732)

`Subscription.groupStart` and `groupEnd` are replaced by `ranges`, a list of `Range` whose ends are `AnchorAbsolute` or `AnchorRelative` (an absolute group and frame, or a count of groups before the latest), and an optional `order`.
To start at group `n`, pass one range starting at the absolute anchor for group `n`, frame 0.

### Receive estimates come from `Session.recvBandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `Session.recvBandwidth` returns to request PROBE, read it with `peek()` or wait with `changed()`, and `cancel()` it to stop.
Replace `stats().estimatedRecvRateBps` with `recvBandwidth()?.peek()`.

### `AnnouncedBroadcast.available()` waits for the next publisher instance (#5700)

The first call returns the broadcast serving the path, as before.
Each later call waits for the next instance to take over the path, instead of returning whatever serves it now.

### A timed track refuses a group that rewinds (#5803)

Writing a frame fails with `TimestampMismatch` when a group's first frame starts before a lower-sequence group's, or after a higher one's.
A publisher whose timeline restarts publishes a new broadcast instead.

### Bandwidth reserves a track's demand (#5845)

`Bandwidth.reserve` takes the track's `demand()` instead of the producer, and no longer throws.

### Media subscribe and fetch take a catalog rendition (#5834)

`TrackConfig` and `GroupConfig` in `dev.moq.media` replace `name` and `container` with a `rendition` (`VideoRendition` or `AudioRendition`) and follow its broadcast and track themselves.

```kotlin
val config = TrackConfig(rendition = VideoRendition(id, video))
```

### Native codecs have `audio` and `video` packages (#5694)

`BroadcastProducer.encodeAudio` / `encodeVideo` are the `dev.moq.audio.Producer` / `dev.moq.video.Producer` constructors, which take a `CatalogProducer`.
The audio track name moves into the encoder output's `track`, and a zero `frameDurationUs` takes the codec's own frame.
`BroadcastConsumer.decodeAudio` / `decodeVideo` are `dev.moq.audio.Consumer.subscribe(broadcast, ...)` / `dev.moq.video.Consumer.subscribe(broadcast, ...)`.
Producers expose subscriber state only through `demand()`, and a video `gop` is `MoqVideoGop.Keyframe(interval)`.
`dev.moq.audio.Encoder` / `dev.moq.video.Encoder` encode without publishing.

```kotlin
val video = dev.moq.video.Producer(CatalogProducer(broadcast), input, output)
val frames = dev.moq.video.Consumer.subscribe(consumer, name, rendition, decoder)
```

### Background panics are reported (#5562)

A panic in a background task throws `uniffi.moq.MoqException.Task` with the panic message as `reason`, and other objects keep running.

### Announcements are events with a restart

`announced(config).updates()` emits an `AnnounceEvent`: `AnnounceEventStart`, `AnnounceEventUpdate`, `AnnounceEventEnd`, or `AnnounceEventRestart`, each carrying an `Announce` as `announce`.
`Moq.announcements()` and `AnnounceUpdate` are gone.
A restart means another publisher instance now serves the prefix: request the path again.

```kotlin
moq.announced().updates().collect { event ->
    when (event) {
        is AnnounceEventStart -> subscribe(event.announce.prefix)
        is AnnounceEventRestart -> subscribe(event.announce.prefix)
        else -> {}
    }
}
```

### "Epoch" means only the publisher epoch (#5146)

`Moq.epoch()` is `connects()`, and `AudioProducer.resetEpoch()` is `reanchor()`.

### `connect` takes a `ClientConfig`

The keyword arguments moved into a `ClientConfig` with nested `tls`, `quic`, `websocket`, and `backoff` records.
The `subscribe` origin is `consume`, and `reconnect = false` is `once = true`.
A value the native side cannot use throws `MoqException.Config`, including a bad bind address.
A server request takes its origins in `accept(publish, consume)`, where `null` inherits the server's origin.

```kotlin
val moq = Moq.connect(url, ClientConfig(tls = ClientTls(insecure = true), consume = origin))
```

### Data tracks wrap a track

`BroadcastProducer.publishJsonSnapshot` / `publishJsonStream`, the binary variants, and the matching consumers are gone.
Create the track, then wrap it with `dev.moq.json` (or `dev.moq.flate` for opaque bytes).

```kotlin
val state = dev.moq.json.SnapshotProducer(broadcast, broadcast.publishTrack("state", null), config)
```

### Media lives in `dev.moq.media`

`BroadcastProducer.publishAudio`, `publishVideo`, `publishContainer`, their `OnTrack` and `Stream` variants, `setVideoProperties`, and `setCatalogSection`, and the consumer's catalog and media subscriptions, are gone.
Use `TrackProducer.audio` / `video`, `ContainerProducer`, `CatalogProducer` (with `setSection`), `CatalogConsumer.subscribe`, `TrackConsumer.subscribe`, and `GroupConsumer.fetch`.
Producers drop `name`, `used`, and `unused`; read them through `demand()`.

```kotlin
val audio = dev.moq.media.TrackProducer.audio(broadcast, Named(null), AudioInit(format = AudioFormat.OPUS, data = opusHead))
```

### Container byte streams use `ContainerProducer` (#5919)

`BroadcastProducer.publishContainerStream` has no separate importer: a container recovers its own framing, so `dev.moq.media.ContainerProducer` takes chunks split anywhere.
Pass empty `data` and write the stream through it.
`ContainerInit`'s optional `timebase` shares one source's offset across container imports.

```kotlin
val stream = ContainerProducer(broadcast, ContainerInit(ContainerFormat.FMP4, byteArrayOf(), null))
```

### Timestamps are optional

`Frame.timestamp` and `Datagram.timestamp` are `Duration?`, null on a frame read from an untimed track.
A raw write to a timed track without a timestamp throws `TimestampMismatch` instead of going out at 0, and whole-frame media writes require one.
`CatalogProducer.now()` samples the broadcast clock for local-event stamps.
Pass `timestampUs` when writing to a timed track, and handle a null `timestamp` when reading.

### Subscriber staleness is `maxDelay`

`Subscription.maxAge`, `AudioDecoderOutput.maxAge`, and `VideoDecoderOutput.maxAge` are `maxDelay`, and the `maxAgeUs` argument is `maxDelayUs`.
`TrackInfo.maxAge` keeps its name: it is the publisher's retention.

### Error payloads are named `reason`

Exceptions that carried a positional string (`Transport`, `Internal`, `Connect`, ...) name it `reason`.
Read `e.reason` instead of `e.v1`.

### `Transport` gains `WEB_TRANSPORT`

A WebTransport session reports `Transport.WEB_TRANSPORT`; `QUIC` now means raw QUIC only, so an exhaustive `when` needs one more branch.
Check for `WEB_TRANSPORT` wherever you checked for `QUIC`.

### `stalled` is `enabled` (#4915)

`Video.stalled` is `enabled`, defaulting to true with the meaning inverted, and `Audio` gains `enabled`.
Replace `!video.stalled` with `video.enabled`.

### Routes have one cost (#4661)

`Route.cold` is removed; drop the `cold` argument.

### Decoded video frames are objects

`VideoDecodedFrame` is a closeable handle with `width()`, `height()`, `pixels(format)`, and `surface()`.
`VideoDecoderOutput.format` is gone; set `surface = true` to keep the decoder's native surface.
Replace `frame.data` with `frame.use { it.pixels(VideoPixelFormat.RGBA) }`.

### `BroadcastProducer.finish()` is removed (#4152)

Call `end()` to finish the broadcast and keep the handle, or `close()` to release it.

## 0.4 -> 0.5

### Durations are `kotlin.time.Duration`

Extension properties expose every microsecond field as a `Duration`: `Frame.timestamp`, `Subscription.maxAge`, `ConnectionStats.rtt`, `Backoff.initial`, and the rest.
Replace the millisecond fields, such as `backoff.initialMs`, with these properties.

### `connect` grows options

`Moq.connect` gains `reconnect`, `backoff`, `maxStreams`, and the WebSocket fallback settings as named arguments.
Pass the origin by name: `Moq.connect(url, publish = origin)`.

### Publish split by kind

`publishMedia*` is `publishAudio`, `publishVideo`, or `publishContainer`, each taking its own init.
The raw encoder paths that used to be `publishAudio` / `publishVideo` are `encodeAudio` / `encodeVideo`.
For example, `publishAudio(AudioInit(format = AudioFormat.OPUS, data = opusHead))`.

### Renames

| Before | After |
| --- | --- |
| `estimatedSendBitrate` / `estimatedRecvBitrate` | `estimatedSendRate` / `estimatedRecvRate` |
| `Announced` / `Announcement` | `AnnounceConsumer` / `AnnounceUpdate` |
| `BroadcastRequest.abort` | `reject` |
| `OriginOptions` | `OriginConfig` |

### Errors are structured

`MoqException.Protocol` carries a `ProtocolError` (scope, wire code, kind) instead of a flattened message.
Setters throw (`Busy` during connect or listen) instead of dropping the value.
Track and group `finish()` keep the handle open, so a later `abort()` can still run.
Read `e.details.kind` and `e.details.code` instead of `e.message`.
