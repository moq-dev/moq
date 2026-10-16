---
title: Swift
description: Async sequences for iOS and macOS via the Moq package
---

# Swift

[![Swift Package Index](https://img.shields.io/github/v/release/moq-dev/moq-swift?label=moq-swift)](https://github.com/moq-dev/moq-swift/releases)

The `Moq` Swift package: de-prefixed types, `AsyncSequence` on every
consumer, `Sendable` handles, and `Task` cancellation that reaches the native
side. It depends on `MoqFFI`, which ships a prebuilt XCFramework with arm64
slices for iOS 16+, the iOS Simulator, and macOS 12.3+.

`Media` owns catalogs, encoded-media importers, and container consumers.
`Media.TrackProducer.audio` / `.video` take a broadcast, an init record, and a
`.named(name:)` or `.requested(request)` target. `Media.CatalogProducer(broadcast:)`
updates catalog properties and sections without keeping the broadcast open.
Its writes fail after closing or releasing the broadcast.
`Flate.SnapshotProducer` / `StreamProducer` publish opaque bytes with an optional `timestampUs`.
A missing data timestamp stays untimed; the library never substitutes zero or now.
Raw tracks with a `nil` timescale are untimed on both publish and receive.
Whole-frame media writes require caller time; container imports keep their own timeline handling.
Timed raw/data tracks require caller-supplied timestamps. Use `Media.CatalogProducer.now()` to stamp a locally
created event on the broadcast clock; reading it fixes that clock for later media producers.
Share `Media.Timebase(broadcast:)` through container init records when several imports
use one source timeline. `ContainerStreamProducer` takes `ContainerStreamInit`;
omitting its timebase keeps an independent offset. A timebase belongs to its broadcast
and does not keep it open.
For arrival-order reads, decode each group from `TrackConsumer.recvGroup()` with
`Media.GroupConsumer(group:container:)`; frames carry container media time as they land.

```swift ignore
dependencies: [
    .package(url: "https://github.com/moq-dev/moq-swift", from: "<version>"),   // latest: see the badge above
],
targets: [
    .target(name: "MyApp", dependencies: [.product(name: "Moq", package: "moq-swift")]),
]
```

```swift
import Moq

// Subscribe. The sequence is live, so run it in its own Task.
let client = try Client()
let session = try await client.connect(to: "https://relay.example.com")

for try await event in try session.consume.announced(prefix: "live/", filter: "*/camera") {
    // A restart is a new publisher run at the same path: request it again.
    let announcement: Announce
    switch event {
    case .start(let started), .restart(let started): announcement = started
    default: continue // .update or .end
    }
    // Prefixes stay origin-relative; captures reports what each wildcard matched.
    print(announcement.captures ?? [])
    let broadcast = try await session.consume.requestBroadcast(path: announcement.prefix)
    for try await catalog in try await Media.CatalogConsumer.subscribe(broadcast: broadcast) {
        print(catalog)
    }
}
```

```swift
// Publish encoded frames, or raw pixels with the codec inside the binding (VideoToolbox).
// opusInit, packet, pts, and rgba come from your encoder or capture source.
let broadcast = try session.publish.createBroadcast(path: "my-stream.hang")
let audio = try Media.TrackProducer.audio(broadcast: broadcast, initData: Media.AudioInit(format: .opus, data: opusInit))
try audio.writeFrame(packet, timestampUs: 20_000)
try audio.cut() // audio has no keyframes, so this is what gives it groups

let video = try broadcast.encodeVideo(
    input: VideoEncoderInput(format: .rgba, width: 1280, height: 720, framerate: 30),
    output: VideoEncoderOutput(codec: .h264, track: "camera", kind: .auto)
)
try video.write(VideoFrame(timestampUs: pts, data: rgba))
try broadcast.announce(route: Route(epoch: mintEpoch())) // a fresh epoch per run
try audio.finish()
try video.finish()
try broadcast.close()

try await session.shutdown()
```

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names.

- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `cut()`: after every frame, or at a segment cadence to align with video.
- **Live encoder timing.** After writing a frame you encoded yourself, call `flush(timestampUs:)` with the same timestamp so the catalog advertises your jitter. Skip it for file and network imports. On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Decoded frames hold decoder buffers.** Release each frame from `decodeVideo` promptly, or the decoder stalls. `resize` is best effort, so read each frame's `width()` and `height()`.
- **Closing.** `try await session.shutdown()` drains for up to one second and throws on failure. `session.cancel(code:)` closes at once. Finish or abort live tracks first. Application delivery requires explicitly selecting `moq-lite-07-wip` on both peers; older Lite versions (including default lite-06) and IETF wait only for transport ACKs.
- **Stats.** `session.stats()` reports `rttUs`, `estimatedSendRateBps`, `estimatedRecvRateBps`, and the byte and packet counters (`bytesSent`, `bytesReceived`, `bytesLost`, `packetsSent`, `packetsReceived`, `packetsLost`). A field is `nil` when the transport does not report it, which is not the same as zero.
- **Errors.** Diagnostic cases use the `reason:` argument label; match their associated value to inspect it. Protocol errors carry `details`.
- **Disabled renditions.** Audio and video convenience decoders reject a catalog configuration with `enabled: false`. Select an enabled rendition from the latest catalog before opening a decoder.
- **Track demand.** Obtain a dynamic track request’s `demand()` before accepting it to watch subscribers through the handoff to its producer. The watch does not keep the track alive and ends when the track closes or the request is rejected.

## Runtime

Native I/O and background tasks run on a Tokio worker pool. Calls awaited by the
host still poll in place; independent native wake callbacks can run concurrently.
The generated bindings resume Swift continuations. Use the appropriate actor
when updating UI state.
A panic in a native background task reaches the pending and later fallible calls
of every object that depends on it as a task error; other objects keep running.

A session closed for exceeding the relay's request caps reports the protocol
kind `ProtocolKind.tooManyRequests`. Reduce
the simultaneous announcements or subscriptions, or ask the relay operator
to adjust the [session limits](/bin/relay/config#session-and-peer), before
reconnecting with the same workload.
A session closed because its credential expired reports `ProtocolKind.expired`, and a request refused for an expired token raises `MoqError.Expired`; refresh the token before retrying.

## Reference

- API reference: [Swift Package Index (DocC)](https://swiftpackageindex.com/moq-dev/moq-swift/documentation/moq)
- Source: [`swift/`](https://github.com/moq-dev/moq/tree/main/swift); `just swift check` builds and tests on a Mac
- Packages SPM resolves: [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift), [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi)
