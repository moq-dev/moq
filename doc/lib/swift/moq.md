---
title: Moq (Swift)
description: Swift Package Manager target for Media over QUIC
---

# Moq

The ergonomic Swift Package Manager target for [Media over QUIC](/).

A Swift-native wrapper over the UniFFI-generated bindings: de-prefixed types, `AsyncSequence` streams, throwing initializers, `Sendable` handles, and Swift-friendly errors. The raw `MoqFFI` types it wraps stay out of your way (data types like `Frame` and `Catalog` are re-exported under de-prefixed names).

Full API reference: [Swift Package Index](https://swiftpackageindex.com/moq-dev/moq-swift/documentation/moq), which builds and hosts the DocC docs from the `///` comments on each tagged release.

## Install

```swift
.package(url: "https://github.com/moq-dev/moq-swift", from: "0.4.4"),
```

Add `Moq` to your target's dependencies:

```swift
.target(
    name: "MyApp",
    dependencies: [
        .product(name: "Moq", package: "moq-swift"),
    ],
),
```

The raw `MoqFFI` bindings and the prebuilt XCFramework are pulled in transitively from [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi); you only depend on `moq-swift`.

Supported platforms: iOS 15+, iPadOS 15+, macOS 12.3+ (ScreenCaptureKit, which the video backend links, ships in 12.3). The XCFramework ships arm64 slices for iOS devices, the iOS Simulator, and macOS.

## Connect

```swift
import Moq

let client = Client()
let session = try await client.connect(to: "https://relay.example.com")
```

`session.publisher` and `session.consumer` are always populated: by whatever origin you wired via `setPublish` / `setConsume` before connecting, or by a fresh auto-created one for any side you left unset. The duplex no-config path (the typical client) shares one origin between both.

For development against a relay with a self-signed certificate:

```swift
let client = Client()
client.setTlsVerify(false)
try client.bind("127.0.0.1:0")
let session = try await client.connect(to: "https://localhost:4443")
```

When you're done, signal graceful shutdown to the peer:

```swift
session.shutdown()  // alias for cancel(code: 0)
```

Inspect an incoming request's logical endpoint before accepting it with the query-free `request.path`. It is consistent across transports and returns `""` for the root or missing path. `request.query` returns the encoded query and may contain credentials:

```swift
let server = Server()
try server.bind("127.0.0.1:4443")
server.generateTls(hostnames: ["localhost"])
_ = try await server.listen()

while let request = try await server.accept() {
    if request.path == "/admin" {
        try await request.reject(code: 403)
        continue
    }
    let session = try await request.accept()
    Task { try? await session.closed() }
}
```

A server can reject the connection on auth grounds: `MoqError.Unauthorized` (HTTP 401) or `MoqError.Forbidden` (HTTP 403). These are terminal: retrying without new credentials won't help, so handle them separately from a transient transport failure. Use the `isAuth` helper to catch both:

```swift
do {
    let session = try await client.connect(url: "https://relay.example.com")
} catch let error as MoqError where error.isAuth {
    // Prompt for credentials; don't reconnect.
}
```

### Reconnecting

The session automatically redials with backoff when the transport drops (a relay
restart, a laptop waking from sleep), and broadcasts consumed through it ride out
the gap. Call `client.setReconnect(false)` before connecting for a one-shot dial,
or `client.setBackoff(...)` to tune the pacing. `session.status()` reports each
transition, and `session.closed()` resolves only once the connection stops for
good:

```swift
while let status = try? await session.status() {
    print("status: \(status)")  // .connected / .disconnected / .migrating
}
```

## Subscribe

Every consumer is an `AsyncSequence`, so iterate directly:

```swift
let announced = try session.consumer.announced(prefix: "demos/")

for try await announcement in announced {
    let catalog = try announcement.broadcast.subscribeCatalog()
    for try await update in catalog {
        print("catalog: \(update)")
    }
}
```

Raw track subscribers can query the publisher's track properties and change their own delivery preferences without resubscribing:

```swift
let track = try await announcement.broadcast.subscribeTrack(
    name: "events",
    subscription: Subscription(priority: 10))
let info = try track.info()
track.update(subscription: Subscription(priority: 20, ordered: false))
```

`ordered` controls prioritization only. When true, groups are prioritized in sequence order. Groups may always arrive out-of-order (or not at all) over the network.

A catalog rendition may name a *different* broadcast: `Video.broadcast` / `Audio.broadcast` is a path
relative to the broadcast the catalog came from, so a transcode output at `live/hd` can describe a
track that actually lives in `live/source`. `decodeAudio` and `decodeVideo` follow it for you.
`subscribeMedia`, `subscribeTrack`, `fetchGroup`, and `fetchMediaGroup` take a track name rather than
a rendition, so resolve first:

```swift
let source = try await announcement.broadcast.resolve(rendition.broadcast)
let consumer = try await source.subscribeMedia(name: name, container: rendition.container)
```

`resolve(nil)` (or an empty reference) returns the same broadcast, so it is safe to call
unconditionally. It needs an origin to fetch a sibling from, so it throws on a broadcast consumed
straight from a local producer.
`resolve` reports a sibling that exists but has not been announced yet as unroutable rather than
waiting for it, so await the referenced broadcast's announcement first if you may be racing it.

## Publish

```swift
let broadcast = try session.publisher.createBroadcast(path: "my-stream")
let audio = try broadcast.publishAudio(format: .opus, initData: opusInitBytes, label: "English")

// Audio has no keyframes, so `cut` is what gives it group boundaries. Once per
// frame is the lowest latency; a segment cadence suits HLS/DASH.
try audio.writeFrame(payload, timestampUs: 0)
try audio.cut()
try audio.writeFrame(payload, timestampUs: 20_000)
try audio.cut()
try audio.finish()
try broadcast.finish()
```

The optional `label` is presentation metadata for a track picker and does not
change the generated transport track name. `publishContainer` takes neither a
label nor a hint: a container describes each track it publishes from its own
metadata, so there is no single rendition for either to name. Video publishers
can pass `hint: VideoHint(...)` to seed catalog fields before the stream reveals
them. Use `publishAudio(on:format:initData:label:)` or
`publishVideo(on:format:initData:label:hint:)` to accept a media track obtained
from `BroadcastDynamic`.

Each catalog `Video` has a `stalled` boolean. A true value recommends temporarily avoiding that rendition, but the track remains directly usable. Existing catalogs default it to false.

Properties that apply to every video rendition are updated together. `nil` fields clear the corresponding catalog property, and rotation is normalized to the nearest clockwise quarter turn:

```swift
try broadcast.setVideoProperties(
    VideoProperties(
        display: Dimensions(width: 1080, height: 1920),
        rotation: 90,
        flip: false
    )
)
```

For sparse or replayed raw tracks, use `track.createGroup(sequence:)`. `track.finish(at:)` declares the exclusive end while still permitting lower groups, and `group.abort(errorCode:)` terminates a group with an application error.

### Fetching raw groups

Fetch retrieves one group by track name and group sequence without keeping a live subscription:

```swift
let group = try await consumer.fetchGroup(
    name: "events",
    sequence: 42,
    options: FetchGroupOptions(priority: 10)
)
for try await frame in group {
    print(frame.timestampUs, frame.payload)
}
```

A retained group resolves immediately. To serve a group that is not retained, keep a dynamic handler alive on its producer:

```swift
let dynamic = try track.dynamic()

for try await request in dynamic {
    let group = try request.accept()
    try group.writeFrame(loadArchivedFrame(request.sequence), timestampUs: request.sequence * 20_000)
    try group.finish()
}
```

Call `request.abort(errorCode:)` when the requested group cannot be produced. Fetch is currently a single-group operation and is supported by the moq-lite 05+ FETCH wire path.

### Fetching media groups

`fetchGroup` hands back raw payloads. `fetchMediaGroup` decodes the same group through the rendition's advertised container, so you get timestamped frames without opening a live subscription:

```swift
let catalog = try await consumer.subscribeCatalog().next()!
let (name, audio) = catalog.audio.first!

let group = try await consumer.fetchMediaGroup(
    name: name,
    sequence: 42,
    container: audio.container
)
for try await frame in group {
    print(frame.timestampUs, frame.payload.count)
}
```

`MediaGroupConsumer` is an `AsyncSequence`, and cancels the native read when iteration ends. A fetched media group is finite: it completes after the group's last decoded frame, unlike the live `subscribeMedia` stream. Latency-based group skipping does not apply, so you always get every frame in the group.

### On-demand raw tracks

Use a dynamic broadcast when subscribers should be able to request raw tracks that are not published yet:

```swift
let broadcast = try session.publisher.createBroadcast(path: "events")
let dynamic = try broadcast.dynamic()

for try await request in dynamic {
    if try request.name == "alerts" {
        let track = try request.accept()
        try track.writeFrame(Data("ready".utf8), timestampUs: 20_000)
        try track.finish()
    } else {
        try request.abort(errorCode: 404)
    }
}
```

Each request arrives as a `TrackRequest`; call `accept(info:)` to turn it into a `TrackProducer` (omit `info` for defaults), or `abort(errorCode:)` to reject the subscriber. Use `writeFrame(_:timestampUs:)` with a presentation timestamp in microseconds. Raw tracks default to a microsecond timescale. Raw consumers receive `Frame` values (payload plus timestamp) from `readFrame()` and group iteration; media subscriptions yield `MediaFrame`, which adds the codec-derived `keyframe` flag.

### Raw datagrams

Raw tracks can send a single best-effort payload without opening a group stream:

```swift
let sequence = try track.appendDatagram(Data("meter update".utf8), timestampUs: 42_000)
let datagram = try await consumer.recvDatagram()

for try await datagram in consumer.datagrams {
    print(datagram.sequence, datagram.timestampUs)
}
```

Datagrams are delivered as `Datagram(sequence, timestampUs, payload)`. Payloads are capped at 1200 bytes. Delivery requires a datagram-capable transport and lite-05 or newer moq-lite; IETF moq-transport, pre-lite-05, WebSocket, and TCP paths do not deliver them, and there is no stream fallback.

### JSON tracks

For JSON payloads, publish and subscribe with the framing handled for you. Values are your own `Codable` types, encoded and decoded at the boundary with `JSONEncoder` / `JSONDecoder`. You opt into one of two distinct modes, one method per mode:

- **Snapshot** (lossy): one value updated over time; a subscriber only sees the latest. Ideal for status documents and metadata. A late joiner catches up to the newest value in one step.
- **Stream** (lossless): an ordered append-log where every record is preserved. Ideal for event logs and timelines.

```swift
struct Status: Codable { var state: String; var viewers: Int }

// Snapshot: each update supersedes the last.
let status = try broadcast.publishJsonSnapshot(name: "status", of: Status.self, compression: true)
try status.update(Status(state: "live", viewers: 42))
try status.update(Status(state: "live", viewers: 43))

let consumer = try broadcast.consume()
for try await value in try await consumer.subscribeJsonSnapshot(name: "status", as: Status.self, compression: true) {
    print(value.viewers)
}

// Stream: every record is delivered in order.
struct Event: Codable { var event: String }
let events = try broadcast.publishJsonStream(name: "events", of: Event.self)
try events.append(Event(event: "started"))

for try await record in try await consumer.subscribeJsonStream(name: "events", as: Event.self) {
    print(record.event)
}
```

`compression` must match on the producer and subscriber. Snapshot mode also takes `deltaRatio` (`0` disables merge-patch deltas, so every change is a fresh snapshot). Advertise the track with a catalog section (`setCatalogSection`) if subscribers should discover it.

### On-demand broadcasts

Use a dynamic origin when consumers should be able to request whole broadcasts that are not announced:

```swift
let origin = OriginProducer(cacheCapacityBytes: 256 * 1024 * 1024)
let dynamic = origin.dynamic()

for try await request in dynamic {
    if try request.path == "events" {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "status")
        try request.accept(broadcast: broadcast)
        try track.writeFrame(Data("ready".utf8), timestampUs: 0)
    } else {
        try request.abort(errorCode: 404)
    }
}
```

The served broadcast is not announced. It only resolves consumers that call `requestBroadcast(path:)`. Each request arrives as a `BroadcastRequest`; call `accept(broadcast:)` to serve it, or `abort(errorCode:)` to fail the requester.

### Raw media

`publishAudio` / `publishVideo` take frames you already encoded. To hand over raw pixels or PCM instead and let the codec run inside the bindings, use `encodeVideo` / `encodeAudio`. Pixel format, resolution, and framerate are fixed at publish time, so each frame carries only its pixels and a timestamp:

```swift
let video = try broadcast.encodeVideo(
    input: VideoEncoderInput(format: .rgba, width: 1280, height: 720, framerate: 30),
    output: VideoEncoderOutput(codec: .h264, bitrate: nil, gop: nil, kind: .auto)
)

try video.write(VideoFrame(timestampUs: ptsUs, data: rgba))
try video.finish()
```

`decodeVideo` and `decodeAudio` are the mirrors: they run the codec inside the bindings on the way in, so a subscriber gets pixels and PCM without linking one. Video frames arrive as tightly-packed I420 and carry the size they actually decoded to, since `resize` is only best effort:

```swift
let catalog = try await broadcast.catalog()
let (name, rendition) = catalog.video.first!

let video = try await broadcast.decodeVideo(name: name, catalogVideo: rendition)
for try await frame in video {
    render(frame.data, frame.width, frame.height)
}
```

An unrecognized codec throws from `decodeVideo` itself; a recognized one no native backend handles throws when the decoder opens. Either way you find out before the first frame.

`kind: .auto` prefers a hardware encoder and falls back to software; `.software`, `.hardware`, and `.named(name: "videotoolbox")` pin the choice. The bindings compile VideoToolbox (macOS), Media Foundation (Windows), and openh264 (software, everywhere); the Linux hardware codecs are a libmoq-only build option. `setBitrate(_:)` retunes the live encoder without forcing a keyframe, cheap enough to drive from a congestion controller.

The track is named after the codec (`.avc3` / `.hev1`) and its catalog rendition is published immediately, read out of the encoder itself, so subscribers discover it through the catalog rather than a name you pick, and can find it before the first frame exists. `cut()` starts a new group at the next frame, which is optional: the encoder keyframes every `gop` frames on its own, and each of those cuts a group.

## Cancellation

All async sequences cooperate with structured concurrency. Cancelling the surrounding `Task` propagates to the underlying `cancel()` on the consumer:

```swift
let task = Task {
    for try await frame in mediaConsumer {
        process(frame)
    }
}

// Later:
task.cancel()   // releases native resources
```

## A note on enum casing

`MoqError` keeps Rust's PascalCase variants, each carrying `message: String` (e.g. `MoqError.Closed(message: "...")`); use `error.isShutdown` to fold the graceful `Cancelled` / `Closed` cases. Plain enums round-trip to lowerCamelCase (`AudioFormat.s16`, `AudioCodec.opus`).

## Local development

To run the test suite, build a host-only XCFramework first:

```bash
just swift check
```

This runs `swift/scripts/check.sh`, which builds `moq-ffi` for the host arch, regenerates the UniFFI Swift bindings, drops a single-slice `MoqFFI.xcframework` into `swift/`, and runs `swift test` against the monolithic local-dev `Package.swift`. Requires macOS with `xcodebuild`.

## See also

- API reference: [Swift Package Index (DocC)](https://swiftpackageindex.com/moq-dev/moq-swift/documentation/moq)
- Source: [swift/Sources/Moq](https://github.com/moq-dev/moq/tree/main/swift/Sources/Moq)
- Mirror repos: [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift) (wrapper), [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi) (raw bindings)
- The Rust crate this wraps: [moq-net](/lib/rs/crate/moq-net)
