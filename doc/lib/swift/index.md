---
title: Swift
description: Async sequences for iOS and macOS via the Moq package
---

# Swift

[![Swift Package Index](https://img.shields.io/github/v/release/moq-dev/moq-swift?label=moq-swift)](https://github.com/moq-dev/moq-swift/releases)

The `Moq` Swift package: de-prefixed types, `AsyncSequence` on every
consumer, `Sendable` handles, and `Task` cancellation that reaches the native
side. It depends on `MoqFFI`, which ships a prebuilt XCFramework with arm64
slices for iOS 15+, the iOS Simulator, and macOS 12.3+.

```swift
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

for try await announcement in try session.consumer.announced(prefix: "live/") {
    for try await catalog in try announcement.broadcast.subscribeCatalog() {
        print(catalog)
    }
}
```

```swift
// Publish encoded frames, or raw pixels with the codec inside the binding (VideoToolbox).
// opusInit, packet, pts, and rgba come from your encoder or capture source.
let broadcast = try session.publisher.createBroadcast(path: "my-stream.hang")
let audio = try broadcast.publishMedia(format: "opus", initData: opusInit)
try audio.writeFrame(packet, timestampUs: 20_000)

let video = try broadcast.publishVideo(
    input: VideoEncoderInput(format: .rgba, width: 1280, height: 720, framerate: 30),
    output: VideoEncoderOutput(codec: .h264, track: "camera", bitrate: nil, gop: nil, kind: .auto)
)
try video.write(VideoFrame(timestampUs: pts, data: rgba))

session.shutdown()
```

For a self-signed relay on your own test network, `client.setTlsVerify(false)`
accepts any certificate; prefer `setTlsRoots` or a fingerprint anywhere else.

`Server` binds, generates or loads TLS, and hands you each request to
`accept()` or `reject(code:)`. JSON tracks take `Codable` types
(`publishJsonSnapshot(name:of:)`, `subscribeJsonStream(name:as:)`), and the
rest of the [shared feature list](/lib/#what-every-binding-can-do) maps one
to one: `fetchGroup`/`fetchMediaGroup`, `dynamic()`, `appendDatagram`/
`datagrams`, `setCatalogSection`, `used()`/`unused()`. `MoqError.isAuth` and
`isShutdown` classify errors.

- API reference: [Swift Package Index (DocC)](https://swiftpackageindex.com/moq-dev/moq-swift/documentation/moq)
- Source: [`swift/`](https://github.com/moq-dev/moq/tree/main/swift); `just swift check` builds and tests on a Mac
- Packages SPM resolves: [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift), [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi)
