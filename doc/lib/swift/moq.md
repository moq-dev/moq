---
title: Moq (Swift)
description: Swift Package Manager target for Media over QUIC
---

# Moq

The Swift Package Manager target for [Media over QUIC](/).

This is an ergonomic wrapper around the UniFFI-generated `MoqFFI` types, providing `AsyncSequence` adapters, Swift-friendly errors, and a `Moq.connect` helper that returns a session you can `await`.

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

let session = try await Moq.connect(url: "https://relay.example.com")
```

For development against a relay using a self-signed certificate, pass `tlsVerify: false`.

## Subscribe

```swift
let consumer = MoqOriginProducer().consume()
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
let broadcast = MoqBroadcastProducer()
let audio = try broadcast.publishMedia(format: "opus", init: opusInitBytes)

try session.publish(path: "my-stream", broadcast: broadcast)

try audio.writeFrame(payload: payload, timestampUs: 0)
try audio.writeFrame(payload: payload, timestampUs: 20_000)
audio.finish()
broadcast.finish()
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
