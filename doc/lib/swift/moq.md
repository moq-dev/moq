---
title: Moq (Swift)
description: Swift Package Manager target for Media over QUIC
---

# Moq

The Swift Package Manager target for [Media over QUIC](/).

This is an ergonomic wrapper around the UniFFI-generated `MoqFFI` types, providing `AsyncSequence` adapters and Swift-friendly errors.

## Install

```swift
.package(url: "https://github.com/moq-dev/moq-swift", from: "0.2.0"),
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

Supported platforms: iOS 15+, iPadOS 15+, macOS 12+. The package ships an XCFramework with iOS device (arm64), iOS Simulator (arm64 + x86_64), and macOS universal slices.

## Connect

```swift
import Moq

// Wire an origin as both publish source and consume sink for the
// typical full-duplex client. Set just one side for a subscribe-only
// or publish-only client.
let origin = MoqOriginProducer()
let client = MoqClient()
client.setPublish(origin: origin)
client.setConsume(origin: origin)

let session = try await client.connect(url: "https://relay.example.com")
```

For development against a relay with a self-signed certificate, configure the client before connecting:

```swift
let client = MoqClient()
client.setTlsDisableVerify(disable: true)
try client.setBind(addr: "127.0.0.1:0")
client.setPublish(origin: origin)
client.setConsume(origin: origin)
let session = try await client.connect(url: "https://localhost:4443")
```

When you're done, signal graceful shutdown to the peer:

```swift
session.shutdown()  // alias for cancel(code: 0)
```

A server can reject the connection on auth grounds: `MoqError.Unauthorized` (HTTP 401) or `MoqError.Forbidden` (HTTP 403). These are terminal: retrying without new credentials won't help, so handle them separately from a transient transport failure. Use the `isAuth` helper to catch both:

```swift
do {
    let session = try await client.connect(url: "https://relay.example.com")
} catch let error as MoqError where error.isAuth {
    // Prompt for credentials; don't reconnect.
}
```

## Subscribe

```swift
let consumer = origin.consume()
let announced = try consumer.announced(prefix: "demos/")

for try await announcement in announced.announcements {
    let catalog = try announcement.broadcast().subscribeCatalog()
    for try await update in catalog.updates {
        print("catalog: \(update)")
    }
}
```

## Publish

```swift
let broadcast = try MoqBroadcastProducer()
let audio = try broadcast.publishMedia(format: "opus", init: opusInitBytes)

try origin.publish(path: "my-stream", broadcast: broadcast)

try audio.writeFrame(payload: payload, timestampUs: 0)
try audio.writeFrame(payload: payload, timestampUs: 20_000)
try audio.finish()
try broadcast.finish()
```

### On-demand raw tracks

Use a dynamic broadcast when subscribers should be able to request raw tracks that are not published yet:

```swift
let broadcast = try MoqBroadcastProducer()
let dynamic = try broadcast.dynamic()

try origin.publish(path: "events", broadcast: broadcast)

for try await track in dynamic.requestedTracks {
    if try track.name() == "alerts" {
        try track.writeFrame(payload: Data("ready".utf8))
        try track.finish()
    } else {
        try track.abort(errorCode: 404)
    }
}
```

## Cancellation

All async sequences cooperate with structured concurrency. Cancelling the surrounding `Task` propagates to the underlying `cancel()` call on the consumer:

```swift
let task = Task {
    for try await frame in mediaConsumer {
        process(frame)
    }
}

// Later:
task.cancel()   // releases native resources
```

## Local development

To run the test suite, build a host-only XCFramework first:

```bash
just check-ffi
```

This runs `swift/scripts/check.sh`, which builds `moq-ffi` for the host arch, regenerates the UniFFI Swift bindings, drops a single-slice `MoqFFI.xcframework` into `swift/`, and then runs `swift test`. Requires macOS with `xcodebuild`.

## See also

- Source: [swift/Sources/Moq](https://github.com/moq-dev/moq/tree/main/swift/Sources/Moq)
- Mirror repo: [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift)
- The Rust crate this wraps: [moq-net](/lib/rs/crate/moq-net)
