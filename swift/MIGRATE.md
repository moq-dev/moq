# Migrating the Swift Moq package

How to update your code for each breaking change, newest first.

## Unreleased

### Receive estimates come from `Session.recvBandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `Session.recvBandwidth` returns to request PROBE, read it with `peek()` or wait with `changed()`, and drop it to stop.

```swift
// Before
let rate = session.stats().estimatedRecvRateBps
// After
let bandwidth = session.recvBandwidth()
let rate = bandwidth?.peek()
```

### Bandwidth reserves a track's demand (#5845)

`Bandwidth.reserve(demand:maxBps:)` takes the track's `demand()` instead of the producer, and no longer throws.

```swift
// Before
let reservation = try bandwidth.reserve(track: track, maxBps: maxBps)
// After
let reservation = bandwidth.reserve(demand: try track.demand(), maxBps: maxBps)
```

### Media subscribe and fetch take a catalog rendition (#5834)

`Media.TrackConfig` and `Media.GroupConfig` replace `name` and `container` with a `Media.Rendition` and follow its broadcast and track themselves.
`Audio.Consumer.subscribe` and `Video.Consumer.subscribe` label the catalog key `id:` instead of `name:`.

```swift
// Before
let config = Media.TrackConfig(name: id, container: video.container)
let decoder = try await Video.Consumer.subscribe(broadcast: consumer, name: id, rendition: video)
// After
let config = Media.TrackConfig(rendition: .video(id: id, config: video))
let decoder = try await Video.Consumer.subscribe(broadcast: consumer, id: id, rendition: video)
```

### Native codecs have `Audio` and `Video` namespaces (#5694)

`BroadcastProducer.encodeAudio` / `encodeVideo` are `Audio.Producer` / `Video.Producer` initializers that take a `Media.CatalogProducer`.
The audio track name moves into the encoder output's `track`, and a zero frame duration takes the codec's own frame.
`BroadcastConsumer.decodeAudio` / `decodeVideo` are `Audio.Consumer.subscribe(broadcast:...)` / `Video.Consumer.subscribe(broadcast:...)`.
Producers expose subscriber state only through `demand()`, and a video GOP is `Video.Gop.keyframe(interval:)`.
`Audio.Encoder` / `Video.Encoder` encode without publishing.

```swift
// Before
let video = try broadcast.encodeVideo(input: input, output: output)
let frames = try await consumer.decodeVideo(name: name, catalogVideo: rendition, output: decoder)
// After
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
// Before
for try await update in try session.consume.announced(prefix: "live/") where update.active() {
    subscribe(update.prefix())
}
// After
for try await event in try session.consume.announced(prefix: "live/") {
    switch event {
    case .start(let announce), .restart(let announce): subscribe(announce.prefix)
    default: continue
    }
}
```

### "Epoch" means only the publisher epoch (#5146)

`Session.epoch()` is `connects()`, and `AudioProducer.resetEpoch()` is `reanchor()`.

```swift
// Before
let n = session.epoch()
try audio.resetEpoch()
// After
let n = session.connects()
try audio.reanchor()
```

### `Client` takes its config at init

The `Client` setters are `Client(...)` arguments, with `tls`, `quic`, `websocket`, and `backoff` records.
`setReconnect(false)` is `once: true`.
A value the native side cannot use throws `MoqError.Config`, including a bad bind address.
`Server` takes its config at init too, and a request takes its origins in `accept(publish:consume:)`, where `nil` inherits the server's origin.
`Session.shutdown()` is `async throws` and drains before returning.

```swift
// Before
let client = Client()
try client.setTlsVerify(false)
try client.setConsume(origin)
// After
let client = try Client(tls: ClientTls(insecure: true), consume: origin)
```

### Data tracks wrap a track

`BroadcastProducer.publishJsonSnapshot` / `publishJsonStream` and the consumer's `subscribeJson*` are gone.
Create the track, then wrap it with `Json` (or `Flate` for opaque bytes).

```swift
// Before
let state = try broadcast.publishJsonSnapshot(name: "state", of: State.self)
// After
let state = try Json.SnapshotProducer<State>(broadcast: broadcast, track: try broadcast.publishTrack(name: "state"))
```

### Media lives in `Media`

`BroadcastProducer.publishAudio`, `publishVideo`, `publishContainer`, their `on:` and stream variants, `setVideoProperties`, and `setCatalogSection`, and the consumer's `subscribeCatalog`, `subscribeMedia`, and `fetchMediaGroup`, are gone.
Use `Media.TrackProducer.audio` / `video`, `Media.ContainerProducer`, `Media.CatalogProducer`, `Media.CatalogConsumer.subscribe`, `Media.TrackConsumer.subscribe`, and `Media.GroupConsumer.fetch`.
The catalog types (`Catalog`, `Video`, `Audio`, `AudioFormat`, ...) moved under `Media`.
Producers drop `name`, `used`, and `unused`; read them through `demand()`.

```swift
// Before
let audio = try broadcast.publishAudio(format: .opus, initData: opusHead)
// After
let audio = try Media.TrackProducer.audio(broadcast: broadcast, initData: Media.AudioInit(format: .opus, data: opusHead))
```

### Container byte-stream importers take an init record

`Media.ContainerStreamProducer` takes a `Media.ContainerStreamInit` holding the format instead of a bare format.
Its optional `timebase`, also on `Media.ContainerInit`, shares one source's offset across container imports.

```swift
// Before
let importer = try Media.ContainerStreamProducer(broadcast: broadcast, format: .fmp4)
// After
let importer = try Media.ContainerStreamProducer(broadcast: broadcast, initData: Media.ContainerStreamInit(format: .fmp4))
```

### Timestamps are optional

Raw `writeFrame(_:timestampUs:)` takes a `UInt64?` defaulting to `nil`, and a frame read from an untimed track has a `nil` `timestampUs`.
A write to a timed track without a timestamp throws `TimestampMismatch` instead of going out at 0, and whole-frame media writes require one.
`Media.CatalogProducer.now()` samples the broadcast clock for local-event stamps.

```swift
// Before
try track.writeFrame(payload)
let ts: UInt64 = frame.timestampUs
// After
try track.writeFrame(payload, timestampUs: nowUs)
let ts: UInt64? = frame.timestampUs
```

### Subscriber staleness is `maxDelayUs`

`Subscription`, `AudioDecoderOutput`, and `VideoDecoderOutput` take `maxDelayUs` instead of `maxAgeUs`.
`TrackInfo.maxAgeUs` keeps its name: it is the publisher's retention.

```swift
// Before
let subscription = Subscription(maxAgeUs: 500_000)
// After
let subscription = Subscription(maxDelayUs: 500_000)
```

### Error payloads are named `reason`

Cases that carried a positional string (`Transport`, `Internal`, `Connect`, ...) take a `reason:` label.

```swift
// Before
throw MoqError.Transport("reset")
// After
throw MoqError.Transport(reason: "reset")
```

### `Transport` gains `.webTransport`

A WebTransport session reports `.webTransport`; `.quic` now means raw QUIC only, so an exhaustive `switch` needs one more case.

```swift
// Before
let quic = request.transport == .quic
// After
let quic = request.transport == .quic || request.transport == .webTransport
```

### `stalled` is `enabled` (#4915)

`Media.Video.stalled` is `enabled`, defaulting to true with the meaning inverted, and `Media.Audio` gains `enabled`.

```swift
// Before
if !video.stalled { select(video) }
// After
if video.enabled { select(video) }
```

### Routes have one cost (#4661)

`Route.cold` is removed.

```swift
// Before
let route = Route(cost: 10, cold: 50)
// After
let route = Route(cost: 10)
```

### Decoded video frames are objects

`VideoDecodedFrame` has `timestampUs()`, `width()`, `height()`, `pixels(format:)`, and `surface()` instead of stored fields.
`VideoDecoderOutput.format` is gone; set `surface: true` to keep the decoder's native surface.

```swift
// Before
let rgba = frame.data
// After
let rgba = try frame.pixels(format: .rgba)
```

### `BroadcastProducer.finish()` is `close()` (#4152)

Close the broadcast instead; tracks already subscribed carry on to their own end.

```swift
// Before
try broadcast.finish()
// After
try broadcast.close()
```

## 0.4 -> 0.5

### Publish split by kind

`publishMedia` is `publishAudio`, `publishVideo`, or `publishContainer`, each taking its own init.
The raw encoder paths that used to be `publishAudio` / `publishVideo` are `encodeAudio` / `encodeVideo`.

```swift
// Before
let media = try broadcast.publishMedia(format: "opus", initData: opusHead)
// After
let media = try broadcast.publishAudio(format: .opus, initData: opusHead)
```

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

```swift
// Before
catch MoqError.Protocol(let message) { print(message) }
client.setPublish(origin)
// After
catch MoqError.Protocol(let details) { print(details.kind, details.code) }
try client.setPublish(origin)
```
