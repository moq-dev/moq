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
let client = Client()
let session = try await client.connect(to: "https://relay.example.com")

for try await event in try session.consume.announced(prefix: "live/", filter: "*/camera") {
    guard case .start(let announcement) = event else { continue } // .update or .end
    // Prefixes stay origin-relative; captures reports what each wildcard matched.
    print(announcement.captures ?? [])
    let broadcast = try await session.consume.requestBroadcast(path: announcement.prefix)
    for try await catalog in try await broadcast.subscribeCatalog() {
        print(catalog)
    }
}
```

```swift
// Publish encoded frames, or raw pixels with the codec inside the binding (VideoToolbox).
// opusInit, packet, pts, and rgba come from your encoder or capture source.
let broadcast = try session.publish.createBroadcast(path: "my-stream.hang")
let audio = try broadcast.publishAudio(format: .opus, initData: opusInit)
try audio.writeFrame(packet, timestampUs: 20_000)
try audio.cut() // audio has no keyframes, so this is what gives it groups

let video = try broadcast.encodeVideo(
    input: VideoEncoderInput(format: .rgba, width: 1280, height: 720, framerate: 30),
    output: VideoEncoderOutput(codec: .h264, track: "camera", kind: .auto)
)
try video.write(VideoFrame(timestampUs: pts, data: rgba))
try broadcast.announce()
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
- **Closing.** `try await session.shutdown()` gives finished tracks up to one second to deliver and throws if they did not. `session.cancel(code:)` closes at once. Finish or abort live tracks first.
- **Stats.** `session.stats()` reports `rttUs`, `estimatedSendRateBps`, `estimatedRecvRateBps`, and the byte and packet counters (`bytesSent`, `bytesReceived`, `bytesLost`, `packetsSent`, `packetsReceived`, `packetsLost`). A field is `nil` when the transport does not report it, which is not the same as zero.

## Reference

- API reference: [Swift Package Index (DocC)](https://swiftpackageindex.com/moq-dev/moq-swift/documentation/moq)
- Source: [`swift/`](https://github.com/moq-dev/moq/tree/main/swift); `just swift check` builds and tests on a Mac
- Packages SPM resolves: [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift), [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi)
