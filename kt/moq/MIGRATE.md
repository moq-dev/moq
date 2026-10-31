# Migrating the Kotlin dev.moq:moq package

How to update your code for each breaking change, newest first.

## Unreleased

### Compression moves to the track

`dev.moq.flate` is removed: publish opaque bytes on a `TrackProducer`.
`SnapshotConfig` and `WindowConfig` lose `compression`, and `StreamConfig` is removed. Publish the track with `TrackInfo(compress = true)` instead.
`StreamProducer` drops its config argument, and `SnapshotConsumer`, `StreamConsumer`, and `WindowConsumer` take only the track.

### Paths with an empty segment are refused ([#6648](https://github.com/moq-dev/moq/pull/6648))

Publishing, serving, or requesting a broadcast at a path with a leading, trailing, or doubled `/` throws instead of collapsing it. Pass the canonical path, such as `room/cam`.

### Request handlers are `requests()` ([#6752](https://github.com/moq-dev/moq/pull/6752))

Rename `dynamic()` on `OriginProducer`, `BroadcastProducer`, `TrackProducer`, and `TrackRequest` to `requests()`.
Their handles `OriginDynamic`, `BroadcastDynamic`, and `TrackDynamic` are `OriginRequests`, `BroadcastRequests`, and `TrackRequests`; their methods and flows are unchanged.

### `Allocator` ([#6751](https://github.com/moq-dev/moq/pull/6751))

`Bandwidth` is `Allocator`, `bandwidth()` is `allocator()`, and `Reservation.grant()` is `rate()`. The producers' `bandwidth` option keeps its name.

### Decoders read gaps and ends

`dev.moq.audio.Consumer.next()` and `dev.moq.video.Consumer.next()` return a `Read` instead of a frame, and their `frames()` flows become `reads()`. Take `(read as? ReadFrame)?.frame` where you used the frame; `ReadGap` marks lost media and `ReadEnd` where the publisher stopped its encoder, and what follows either does not continue what came before.

### Durations are `java.time.Duration`

The records carry durations as `java.time.Duration` fields instead of `*Us` microsecond integers, so the duration extension properties are gone: read `backoff.initial`, `stats.rtt`, `grant.expires`, `info.maxAge`, `fill.maxDelay()`, and `consumer.delay()` directly.
Convert with `toKotlinDuration()` and `toJavaDuration()` where you want `kotlin.time.Duration`.
`AudioEncoderOutput.frameDuration` is nullable: pass null where you passed `frameDurationUs = 0u` for the codec's own frame.
`Subscription.maxDelay` defaults to null, which sets no subscriber limit: start from `Subscription.default()`, or pass `java.time.Duration.ZERO`, for the old default.
`TrackConsumer.info()` no longer throws.

### Connection state

`Moq.connect` returns a connecting native session. Replace `status()`, `connects()`, and failure polls with `session.state()` and `session.stateChanged(state.revision)`. `ConnectionState` carries `phase`, `connects`, `failure`, and `version`; `ConnectionPhase` replaces `ConnectionStatus`.

### Exact encoder rates and publication options

Raw video input framerates are exact numerator/denominator records, not integers.
Use `video.FrameRate(30000u, 1001u)` and construct producers with `(catalog, ProducerOptions(input, output, track, bandwidth))` under the matching audio/video namespace.
The codec-only output records no longer contain track naming. Rebuild against
the matching generated bindings and native library.

### Video rendition preference

`dev.moq.media.Video` gains `preference: Int`, defaulting to `0`. Update positional constructors and use the matching moq-ffi package. Higher values are preferred; compatibility transcodes use `-1`.

### `announce` takes an `AnnounceRoute`

`BroadcastProducer.announce` takes `AnnounceRoute(cost, epoch)` instead of a `Route`: the origin builds the hop chain, so `hops` is no longer an input.
`Route` is only what announcements report. To re-announce an observed route, copy its `cost` and `epoch`.

### A broadcast reference waits for its sibling's announcement (#6254)

Resolving a rendition's `broadcast` reference in the rendition-based subscribes and fetches now waits for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, bound the call yourself: cancel the coroutine or wrap it in `withTimeout`.

### Shutdown drains

`session.shutdown()` still returns at once, but it now requests a draining close instead of cancelling.
Await the suspending `session.closed()` to wait for the drain and catch delivery failures.
`Moq.close()` now cancels at once; to drain a `Moq`, call `moq.shutdown()`, await `moq.session.closed()`, then close it.
If you adopted the release line's async `drain()`, call `shutdown()` and then await `closed()` instead.

### Media track names belong to the target

Audio and video init records no longer carry a track name. Pass that name on the media producer's named target instead; init records describe only the codec and media.

### Broadcast `finish` is removed

`BroadcastProducer.finish()`, a deprecated alias for `end()`, is removed.
Call `BroadcastProducer.end()` where you called `finish()`.
To end the content first, call `CatalogProducer.finish()`; the broadcast stays up until `end()`.

### JSON snapshot `deltaRatio` is `deltas`

`SnapshotConfig.deltaRatio: UInt` is replaced by `deltas: Boolean` (default `true`).
Pass `deltas = false` where you passed `deltaRatio = 0u`, and drop the argument otherwise.
Publish the track with a `refresh` ratio so a joiner gets the value restated in a fresh group.

### Priorities are signed (#6142)

`TrackInfo.priority`, `Subscription.priority`, and `FetchGroupOptions.priority` are `Byte`, higher first, and 0 is the unset midpoint; write `priority = 3` instead of `3u`.
`TrackInfo.priority` defaulted to 127 and now defaults to 0.
To keep a value's moq-lite byte, subtract 128 from it: 255 becomes 127, 128 becomes 0, and 0 becomes -128.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `announced()` or `announcedBroadcast()`, instead of under its whole scope at connect.
`requestBroadcast()` watches nothing, so on a client it fails with an unroutable error for a path nothing watches, even when the relay announces it.
Resolve the path with `announcedBroadcast()`, or hold `announced()` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.

### `ContainerFormat.FMP4` unwraps to Legacy (#6250)

A container import with `ContainerFormat.FMP4` now publishes one Legacy frame per sample instead of one CMAF frame per fragment.
Pass the new `ContainerFormat.CMAF` to keep publishing CMAF.

### Catalog references carry their epoch

`Video` and `Audio` gain `epoch`, the publisher instance their `broadcast` reference names; it defaults to `null`, so only positional construction changes.
`BroadcastConsumer.resolve(reference)` becomes `resolve(rendition)`, taking the catalog `Rendition` and reading its `broadcast` and `epoch` from it.
Resolving, and the rendition-based consumers, throw `MoqException.Mux` for a rendition whose broadcast is proven to serve another epoch.

### Timestamps stop at 2^61 - 1 microseconds (#6168)

Writing a frame, or any other call that takes a timestamp, fails with `TimeOverflow` past `2^61 - 1` microseconds instead of `2^62 - 1`, the most moq-lite-05/06 can carry.
A received timestamp past it fails the group as a malformed track.
Clamp timestamps you build from unbounded input to `2^61 - 1`.

### Finished tracks stay until the broadcast ends (#6157)

A finished track now stays listed, with its catalog rendition and demand, until its broadcast ends.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.

### A claim answers by publishing (#5678)

`OriginProducer.dynamic(prefix)` takes no route, and `OriginDynamic.update` is gone.
Replace `request.accept(broadcast)` with `request.prepare(mintEpoch())`, which returns a `BroadcastPrepared`: fill its `broadcast()`, then `publish()` announces it at the exact path under that epoch and returns its producer.

### Anchors can name a time (#6074)

`Anchor` gains `AnchorTime`, so exhaustive `when` expressions need a branch for it.
`TrackConsumer.update` now throws `MoqException` on a time anchor an untimed track can't serve.

### Rebuild the matching raw bindings

The documentation update changes UniFFI's audio-producer method metadata checksums.
Regenerate the Kotlin sources and update the native libraries in `moq-ffi` together,
then rebuild the application; older generated guards reject the rebuilt library.
Application calls are unchanged.

### JSON consumers emit timestamps (#5677)

`values()` emits `json.Value` (`value`, plus `timestamp` as a `Duration?`) instead of a string, and `valuesAs<T>()` emits `Timed<T>(value, timestamp)`.
A snapshot consumer's `next()` now yields every state in order; call `latest()` for the newest.
Timestamps are rounded down to a whole microsecond.

### Ranges filter reads (#5929)

A subscription now reads only the groups its own `ranges` cover.

### Track demand reports the wanted group (#5930)

`TrackDemand.isUsed()`, `used()`, and `unused()` are gone.
`wanted()` returns null while nobody subscribes, otherwise the group subscribers want next, and `wantedChanged()` waits for it to change (the first call returns at once).
Loop on `wantedChanged()` until it returns non-null where you called `used()`, and null where you called `unused()`.
`TrackInfo` gains `refresh`.

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
Hold the consumer `Session.recvBandwidth` returns to request PROBE, read it with `rate()` or wait with `rateChanged()`, and `cancel()` it to stop.
Replace `stats().estimatedRecvRateBps` with `recvBandwidth()?.rate()`.

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
The audio track name belongs to the producer's publication options, and a zero `frameDurationUs` takes the codec's own frame.
`BroadcastConsumer.decodeAudio` / `decodeVideo` are `dev.moq.audio.Consumer.subscribe(broadcast, ...)` / `dev.moq.video.Consumer.subscribe(broadcast, ...)`.
Producers expose subscriber state only through `demand()`, and a video `gop` is `MoqVideoGop.Keyframe(interval)`.
`dev.moq.audio.Encoder` / `dev.moq.video.Encoder` encode without publishing.

```kotlin
val video = dev.moq.video.Producer(CatalogProducer(broadcast), dev.moq.video.ProducerOptions(input, output))
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

An omitted timescale now publishes a raw track untimed instead of selecting microseconds. Explicitly set a microsecond timescale to preserve the released default.

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
