# Migrating the Kotlin dev.moq:moq package

How to update your code for each breaking change, newest first.

## Unreleased

### Receive estimates come from `Session.recvBandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `Session.recvBandwidth` returns to request PROBE, read it with `peek()` or wait with `changed()`, and `cancel()` it to stop.

```kotlin
// Before
val rate = session.stats().estimatedRecvRateBps
// After
val bandwidth = session.recvBandwidth()
val rate = bandwidth?.peek()
```

### `AnnouncedBroadcast.available()` waits for the next publisher instance (#5700)

The first call returns the broadcast serving the path, as before.
Each later call waits for the next instance to take over the path, instead of returning whatever serves it now.

### A timed track refuses a group that rewinds (#5803)

Writing a frame fails with `TimestampMismatch` when a group's first frame starts before a lower-sequence group's, or after a higher one's.
A publisher whose timeline restarts publishes a new broadcast instead.

### Bandwidth reserves a track's demand (#5845)

`Bandwidth.reserve` takes the track's `demand()` instead of the producer, and no longer throws.

```kotlin
// Before
val reservation = bandwidth.reserve(track, maxBps)
// After
val reservation = bandwidth.reserve(track.demand(), maxBps)
```

### Media subscribe and fetch take a catalog rendition (#5834)

`TrackConfig` and `GroupConfig` in `dev.moq.media` replace `name` and `container` with a `rendition` (`VideoRendition` or `AudioRendition`) and follow its broadcast and track themselves.

```kotlin
// Before
val config = TrackConfig(name = id, container = video.container)
// After
val config = TrackConfig(rendition = VideoRendition(id, video))
```

### Native codecs have `audio` and `video` packages (#5694)

`BroadcastProducer.encodeAudio` / `encodeVideo` are the `dev.moq.audio.Producer` / `dev.moq.video.Producer` constructors, which take a `CatalogProducer`.
The audio track name moves into the encoder output's `track`, and a zero `frameDurationUs` takes the codec's own frame.
`BroadcastConsumer.decodeAudio` / `decodeVideo` are `dev.moq.audio.Consumer.subscribe(broadcast, ...)` / `dev.moq.video.Consumer.subscribe(broadcast, ...)`.
Producers expose subscriber state only through `demand()`, and a video `gop` is `MoqVideoGop.Keyframe(interval)`.
`dev.moq.audio.Encoder` / `dev.moq.video.Encoder` encode without publishing.

```kotlin
// Before
val video = broadcast.encodeVideo(input, output)
val frames = consumer.decodeVideo(name, rendition, decoder)
// After
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
// Before
moq.announcements().collect { update ->
    if (update.active()) subscribe(update.prefix())
}
// After
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

```kotlin
// Before
val n = moq.epoch()
audio.resetEpoch()
// After
val n = moq.connects()
audio.reanchor()
```

### `connect` takes a `ClientConfig`

The keyword arguments moved into a `ClientConfig` with nested `tls`, `quic`, `websocket`, and `backoff` records.
The `subscribe` origin is `consume`, and `reconnect = false` is `once = true`.
A value the native side cannot use throws `MoqException.Config`, including a bad bind address.
A server request takes its origins in `accept(publish, consume)`, where `null` inherits the server's origin.

```kotlin
// Before
val moq = Moq.connect(url, tlsVerify = false, subscribe = origin)
// After
val moq = Moq.connect(url, ClientConfig(tls = ClientTls(insecure = true), consume = origin))
```

### Data tracks wrap a track

`BroadcastProducer.publishJsonSnapshot` / `publishJsonStream`, the binary variants, and the matching consumers are gone.
Create the track, then wrap it with `dev.moq.json` (or `dev.moq.flate` for opaque bytes).

```kotlin
// Before
val state = broadcast.publishJsonSnapshot("state", config)
// After
val state = dev.moq.json.SnapshotProducer(broadcast, broadcast.publishTrack("state", null), config)
```

### Media lives in `dev.moq.media`

`BroadcastProducer.publishAudio`, `publishVideo`, `publishContainer`, their `OnTrack` and `Stream` variants, `setVideoProperties`, and `setCatalogSection`, and the consumer's catalog and media subscriptions, are gone.
Use `TrackProducer.audio` / `video`, `ContainerProducer`, `CatalogProducer` (with `setSection`), `CatalogConsumer.subscribe`, `TrackConsumer.subscribe`, and `GroupConsumer.fetch`.
Producers drop `name`, `used`, and `unused`; read them through `demand()`.

```kotlin
// Before
val audio = broadcast.publishAudio(AudioInit(format = AudioFormat.OPUS, data = opusHead))
// After
val audio = dev.moq.media.TrackProducer.audio(broadcast, Named(null), AudioInit(format = AudioFormat.OPUS, data = opusHead))
```

### Container byte-stream importers take an init record

`ContainerStreamProducer` takes a `ContainerStreamInit` holding the format instead of a bare format.
Its optional `timebase`, also on `ContainerInit`, shares one source's offset across container imports.

```kotlin
// Before
val importer = ContainerStreamProducer(broadcast, ContainerFormat.FMP4)
// After
val importer = ContainerStreamProducer(broadcast, ContainerStreamInit(ContainerFormat.FMP4))
```

### Timestamps are optional

`Frame.timestamp` and `Datagram.timestamp` are `Duration?`, null on a frame read from an untimed track.
A raw write to a timed track without a timestamp throws `TimestampMismatch` instead of going out at 0, and whole-frame media writes require one.
`CatalogProducer.now()` samples the broadcast clock for local-event stamps.

```kotlin
// Before
track.writeFrame(Frame(payload = bytes))
val ts: Duration = frame.timestamp
// After
track.writeFrame(Frame(payload = bytes, timestampUs = nowUs))
val ts: Duration? = frame.timestamp
```

### Subscriber staleness is `maxDelay`

`Subscription.maxAge`, `AudioDecoderOutput.maxAge`, and `VideoDecoderOutput.maxAge` are `maxDelay`.
`TrackInfo.maxAge` keeps its name: it is the publisher's retention.

```kotlin
// Before
val subscription = Subscription(maxAgeUs = 500_000u)
val budget = subscription.maxAge
// After
val subscription = Subscription(maxDelayUs = 500_000u)
val budget = subscription.maxDelay
```

### Error payloads are named `reason`

Exceptions that carried a positional string (`Transport`, `Internal`, `Connect`, ...) name it `reason`.

```kotlin
// Before
catch (e: MoqException.Transport) { log(e.v1) }
// After
catch (e: MoqException.Transport) { log(e.reason) }
```

### `Transport` gains `WEB_TRANSPORT`

A WebTransport session reports `Transport.WEB_TRANSPORT`; `QUIC` now means raw QUIC only, so an exhaustive `when` needs one more branch.

```kotlin
// Before
val quic = request.transport() == Transport.QUIC
// After
val quic = request.transport() == Transport.QUIC || request.transport() == Transport.WEB_TRANSPORT
```

### `stalled` is `enabled` (#4915)

`Video.stalled` is `enabled`, defaulting to true with the meaning inverted, and `Audio` gains `enabled`.

```kotlin
// Before
if (!video.stalled) select(video)
// After
if (video.enabled) select(video)
```

### Routes have one cost (#4661)

`Route.cold` is removed.

```kotlin
// Before
val route = Route(cost = 10u, cold = 50u)
// After
val route = Route(cost = 10u)
```

### Decoded video frames are objects

`VideoDecodedFrame` is a closeable handle with `width()`, `height()`, `pixels(format)`, and `surface()`.
`VideoDecoderOutput.format` is gone; set `surface = true` to keep the decoder's native surface.

```kotlin
// Before
val rgba = frame.data
// After
val rgba = frame.use { it.pixels(VideoPixelFormat.RGBA) }
```

### `BroadcastProducer.finish()` is removed (#4152)

Call `end()` to finish the broadcast and keep the handle, or `close()` to release it.

```kotlin
// Before
broadcast.finish()
// After
broadcast.end()
```

## 0.4 -> 0.5

### Durations are `kotlin.time.Duration`

Extension properties expose every microsecond field as a `Duration`: `Frame.timestamp`, `Subscription.maxAge`, `ConnectionStats.rtt`, `Backoff.initial`, and the rest.

```kotlin
// Before
val delayMs = backoff.initialMs
// After
val delay = backoff.initial
```

### `connect` grows options

`Moq.connect` gains `reconnect`, `backoff`, `maxStreams`, and the WebSocket fallback settings as named arguments.

```kotlin
// Before
val moq = Moq.connect(url, ..., origin)
// After
val moq = Moq.connect(url, publish = origin)
```

### Publish split by kind

`publishMedia*` is `publishAudio`, `publishVideo`, or `publishContainer`, each taking its own init.
The raw encoder paths that used to be `publishAudio` / `publishVideo` are `encodeAudio` / `encodeVideo`.

```kotlin
// Before
val media = broadcast.publishMedia(Init(format = "opus", data = opusHead, video = null))
// After
val media = broadcast.publishAudio(AudioInit(format = AudioFormat.OPUS, data = opusHead))
```

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

```kotlin
// Before
catch (e: MoqException.Protocol) { log(e.message) }
// After
catch (e: MoqException.Protocol) { log("${e.details.kind} ${e.details.code}") }
```
