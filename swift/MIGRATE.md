# Migrating the Swift Moq package

How to update your code for each breaking change, newest first.

## Unreleased

### A broadcast reference waits for its sibling's announcement (#6254)

Resolving a rendition's `broadcast` reference, through `BroadcastConsumer.resolve(_:epoch:)` and the rendition-based subscribes and fetches, now waits for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, bound the call yourself: cancel the task.

### JSON snapshot `deltaRatio` is `deltas`

`Json.SnapshotProducer(deltaRatio:)` and `MoqJsonSnapshotConfig.deltaRatio` are replaced by `deltas: Bool` (default `true`).
Pass `deltas: false` where you passed `deltaRatio: 0`, and drop the argument otherwise.
Publish the track with a `refresh` ratio so a joiner gets the value restated in a fresh group.

### Priorities are signed (#6142)

`TrackInfo.priority`, `Subscription.priority`, `FetchGroupOptions.priority`, and `GroupRequest.priority` are `Int8`, higher first, and 0 is the unset midpoint.
`TrackInfo.priority` defaulted to 127 and now defaults to 0.
To keep a value's moq-lite byte, subtract 128 from it, widening first so it doesn't trap (`Int8(Int16(old) - 128)`): 255 becomes 127, 128 becomes 0, and 0 becomes -128.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `announced()` or `announcedBroadcast()`, instead of under its whole scope at connect.
`OriginConsumer.requestBroadcast()` watches nothing, so on a client it fails with an unroutable error for a path nothing watches, even when the relay announces it.
Resolve the path with `announcedBroadcast()`, or hold `announced()` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.

### `ContainerFormat.fmp4` unwraps to Legacy (#6250)

A container import with `ContainerFormat.fmp4` now publishes one Legacy frame per sample instead of one CMAF frame per fragment.
Pass the new `ContainerFormat.cmaf` to keep publishing CMAF.

### Catalog references carry their epoch

`Media.Video` and `Media.Audio` gain `epoch`, the publisher instance their `broadcast` reference names.
`BroadcastConsumer.resolve(_:)` becomes `resolve(_:epoch:)`; pass the rendition's `epoch` alongside its `broadcast`, or `nil` to resolve whichever instance answers.
Resolving, and the rendition-based consumers, throw for a rendition whose broadcast is proven to serve another epoch.

### Timestamps stop at 2^61 - 1 microseconds (#6168)

Writing a frame, or any other call that takes a timestamp, fails with `TimeOverflow` past `2^61 - 1` microseconds instead of `2^62 - 1`, the most moq-lite-05/06 can carry.
A received timestamp past it fails the group as a malformed track.
Clamp timestamps you build from unbounded input to `2^61 - 1`.

### Finished tracks stay until the broadcast ends (#6157)

A finished track now stays listed, with its catalog rendition and demand, until its broadcast closes or the new `BroadcastProducer.finish()` lets go.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.

### A claim answers by publishing (#5678)

`OriginProducer.dynamic(prefix:)` takes no route, and `OriginDynamic.update(route:)` is gone.
Replace `request.accept(broadcast:)` with `let prepared = try request.prepare(epoch: mintEpoch())`, fill `prepared.broadcast`, then `try prepared.publish()`, which announces it at the exact path under that epoch and returns its producer.

### Anchors can name a time (#6074)

`Subscription.Anchor` gains `.time(timestampUs:)`, so exhaustive switches need a case for it.
`TrackConsumer.update(subscription:)` now `throws`, on a time anchor an untimed track can't serve; call it with `try`.

### Rebuild the matching raw bindings

The documentation update changes UniFFI's audio-producer method metadata checksums.
Regenerate the Swift sources and rebuild the `MoqFFI` XCFramework together,
then rebuild the application; older generated guards reject the rebuilt library.
Application calls are unchanged.

### JSON consumers yield `Json.Timed` (#5677)

`Json.SnapshotConsumer` and `Json.StreamConsumer` yield `Json.Timed<Value>` (`value`, `timestampUs`) instead of the bare value; `timestampUs` is `nil` when untimed.
A snapshot consumer's `next()` now yields every state in order; call `latest()` for the newest.
Timestamps are rounded down to a whole microsecond.

### Ranges filter reads (#5929)

A subscription now reads only the groups its own `ranges` cover.

### Track demand reports the wanted group (#5930)

`TrackDemand.isUsed`, `used()`, and `unused()` are gone.
`wanted` is nil while nobody subscribes, otherwise the group subscribers want next, and `wantedChanged()` waits for it to change (the first call returns at once).
Loop on `wantedChanged()` until it returns non-nil where you awaited `used()`, and nil where you awaited `unused()`.
`TrackInfo` gains `refresh`. `GroupDemand` is unchanged.

### `CongestionControl` gained `none` (#5926)

`CongestionControl.none` runs no congestion control and no pacing, for tests and controlled links.
An exhaustive `switch` needs one more case.
Spell it out as `CongestionControl.none` when assigning to `QuicConfig.congestionControl`: a bare `.none` there is `nil`, the backend's default.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Set `QuicConfig.keylog` to `true` as well to capture them; without `SSLKEYLOGFILE` the client or server refuses to build.

### Subscriptions carry group ranges (#5732)

`Subscription.groupStart` and `groupEnd` are replaced by `ranges`, a list of `Subscription.Range` whose ends are a `Subscription.Anchor` (`.absolute(group:frame:)` or `.relative(groups:)`), and an optional `order`.
To start at group `n`, pass one range starting at the absolute anchor for group `n`, frame 0.

### Receive estimates come from `Session.recvBandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `Session.recvBandwidth` returns to request PROBE, read it with `peek()` or wait with `changed()`, and drop it to stop.
It returns `nil` when the version has no PROBE.

### `AnnouncedBroadcast.available()` waits for the next publisher instance (#5700)

The first call returns the broadcast serving the path, as before.
Each later call waits for the next instance to take over the path, instead of returning whatever serves it now.

### A timed track refuses a group that rewinds (#5803)

Writing a frame fails with `TimestampMismatch` when a group's first frame starts before a lower-sequence group's, or after a higher one's.
A publisher whose timeline restarts publishes a new broadcast instead.

### Bandwidth reserves a track's demand (#5845)

`Bandwidth.reserve(demand:maxBps:)` takes the track's `demand()` instead of the producer, and no longer throws.

### Media subscribe and fetch take a catalog rendition (#5834)

`Media.TrackConfig` and `Media.GroupConfig` replace `name` and `container` with a `Media.Rendition` and follow its broadcast and track themselves.
`Audio.Consumer.subscribe` and `Video.Consumer.subscribe` label the catalog key `id:` instead of `name:`.

```swift
let config = Media.TrackConfig(rendition: .video(id: id, config: video))
```

### Native codecs have `Audio` and `Video` namespaces (#5694)

`BroadcastProducer.encodeAudio` / `encodeVideo` are `Audio.Producer` / `Video.Producer` initializers that take a `Media.CatalogProducer`.
The audio track name moves into the encoder output's `track`, and a zero frame duration takes the codec's own frame.
`BroadcastConsumer.decodeAudio` / `decodeVideo` are `Audio.Consumer.subscribe(broadcast:...)` / `Video.Consumer.subscribe(broadcast:...)`.
Producers expose subscriber state only through `demand()`, and a video GOP is `Video.Gop.keyframe(interval:)`.
`Audio.Encoder` / `Video.Encoder` encode without publishing.

```swift
let video = try Video.Producer(catalog: Media.CatalogProducer(broadcast: broadcast), input: input, output: output)
let frames = try await Video.Consumer.subscribe(broadcast: consumer, id: name, rendition: rendition, output: decoder)
```

### Background panics are reported (#5562)

A panic in a background task throws `MoqError.Task` with the panic message as `reason`, and other objects keep running.

### Announcements are events with a restart

`AnnounceConsumer` yields an `AnnounceEvent`: `.start`, `.update`, `.end`, or `.restart`, each carrying an `Announce`.
`AnnounceUpdate` is gone.
A restart means another publisher instance now serves the prefix: request the path again.

```swift
for try await event in try session.consume.announced(prefix: "live/") {
    switch event {
    case .start(let announce), .restart(let announce): subscribe(announce.prefix)
    default: continue
    }
}
```

### "Epoch" means only the publisher epoch (#5146)

`Session.epoch()` is `connects()`, and `AudioProducer.resetEpoch()` is `reanchor()`.

### `Client` takes its config at init

The `Client` setters are `Client(...)` arguments, with `tls`, `quic`, `websocket`, and `backoff` records.
`setReconnect(false)` is `once: true`.
A value the native side cannot use throws `MoqError.Config`, including a bad bind address.
`Server` takes its config at init too, and a request takes its origins in `accept(publish:consume:)`, where `nil` inherits the server's origin.
`Session.shutdown()` is `async throws` and drains before returning.

```swift
let client = try Client(tls: ClientTls(insecure: true), consume: origin)
```

### Data tracks wrap a track

`BroadcastProducer.publishJsonSnapshot` / `publishJsonStream` and the consumer's `subscribeJson*` are gone.
Create the track, then wrap it with `Json` (or `Flate` for opaque bytes).

```swift
let state = try Json.SnapshotProducer<State>(broadcast: broadcast, track: try broadcast.publishTrack(name: "state"))
```

### Media lives in `Media`

`BroadcastProducer.publishAudio`, `publishVideo`, `publishContainer`, their `on:` and stream variants, `setVideoProperties`, and `setCatalogSection`, and the consumer's `subscribeCatalog`, `subscribeMedia`, and `fetchMediaGroup`, are gone.
Use `Media.TrackProducer.audio` / `video`, `Media.ContainerProducer`, `Media.CatalogProducer`, `Media.CatalogConsumer.subscribe`, `Media.TrackConsumer.subscribe`, and `Media.GroupConsumer.fetch`.
The catalog types (`Catalog`, `Video`, `Audio`, `AudioFormat`, ...) moved under `Media`.
Producers drop `name`, `used`, and `unused`; read them through `demand()`.

```swift
let audio = try Media.TrackProducer.audio(broadcast: broadcast, initData: Media.AudioInit(format: .opus, data: opusHead))
```

### Container byte streams use `Media.ContainerProducer` (#5919)

`BroadcastProducer.publishContainerStream` has no separate importer: a container recovers its own framing, so `Media.ContainerProducer` takes chunks split anywhere.
Pass empty `data` and write the stream through it.
`Media.ContainerInit`'s optional `timebase` shares one source's offset across container imports.

```swift
let stream = try Media.ContainerProducer(broadcast: broadcast, initData: Media.ContainerInit(format: .fmp4, data: Data()))
```

### Timestamps are optional

Raw `writeFrame(_:timestampUs:)` takes a `UInt64?` defaulting to `nil`, and a frame read from an untimed track has a `nil` `timestampUs`.
A write to a timed track without a timestamp throws `TimestampMismatch` instead of going out at 0, and whole-frame media writes require one.
`Media.CatalogProducer.now()` samples the broadcast clock for local-event stamps.
Pass `timestampUs:` on every write to a timed track, and handle a `nil` `timestampUs` on read.

### Subscriber staleness is `maxDelayUs`

`Subscription`, `AudioDecoderOutput`, and `VideoDecoderOutput` take `maxDelayUs` instead of `maxAgeUs`.
`TrackInfo.maxAgeUs` keeps its name: it is the publisher's retention.

### Error payloads are named `reason`

Cases that carried a positional string (`Transport`, `Internal`, `Connect`, ...) take a `reason:` label.

### `Transport` gains `.webTransport`

A WebTransport session reports `.webTransport`; `.quic` now means raw QUIC only, so an exhaustive `switch` needs one more case.
Check for `.webTransport` too where you matched `.quic`.

### `stalled` is `enabled` (#4915)

`Media.Video.stalled` is `enabled`, defaulting to true with the meaning inverted, and `Media.Audio` gains `enabled`.
Read `enabled` where you read `!stalled`.

### Routes have one cost (#4661)

`Route.cold` is removed; pass only `cost:`.

### Decoded video frames are objects

`VideoDecodedFrame` has `timestampUs()`, `width()`, `height()`, `pixels(format:)`, and `surface()` instead of stored fields.
`VideoDecoderOutput.format` is gone; set `surface: true` to keep the decoder's native surface.
Call `try frame.pixels(format: .rgba)` instead of reading `frame.data`.

### `BroadcastProducer.finish()` is `close()` (#4152)

Close the broadcast instead; tracks already subscribed carry on to their own end.

## 0.4 -> 0.5

### Publish split by kind

`publishMedia` is `publishAudio`, `publishVideo`, or `publishContainer`, each taking its own init.
The raw encoder paths that used to be `publishAudio` / `publishVideo` are `encodeAudio` / `encodeVideo`.

### Renames

| Before | After |
| --- | --- |
| millisecond durations | microseconds (`maxAgeUs`, `Backoff.initialUs`) |
| `estimatedSendBitrate` / `estimatedRecvBitrate` | `estimatedSendRate` / `estimatedRecvRate` |
| `setTlsDisableVerify(true)` | `setTlsVerify(false)` |
| `Announced` / `Announcement` | `AnnounceConsumer` / `AnnounceUpdate` |
| `BroadcastRequest.abort` | `reject` |
| `OriginOptions` | `OriginConfig` |

### Errors are structured

`MoqError.Protocol` carries `details` (scope, wire code, kind) instead of a flattened message.
Setters throw (`.Busy` during connect or listen) instead of dropping the value.
Track and group `finish()` keep the handle open, so a later `abort()` can still run.
Match `MoqError.Protocol(let details)` and read `details.kind` and `details.code`, and call setters with `try`.
