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

let client = MoqClient()
let cs = try await client.connect(url: "https://cdn.moq.dev/anon/demo")
```

`MoqClient.connect(url:)` returns a `MoqSession`. The accessors `cs.publisher()` and `cs.consumer()` are always populated: by whatever origin you wired via `setPublish` / `setConsume` before connect, or by a fresh auto-created one for any side you didn't set.

For development against a relay with a self-signed certificate, configure the client before connecting:

```swift
let client = MoqClient()
client.setTlsDisableVerify(disable: true)
try client.setBind(addr: "127.0.0.1:0")
let cs = try await client.connect(url: "https://localhost:4443")
```

When you're done, signal graceful shutdown to the peer:

```swift
cs.shutdown()  // alias for cancel(code: 0)
```

## Subscribe

```swift
let announced = try cs.consumer().announced(prefix: "demos/")

for try await announcement in announced {
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

try cs.publisher().announce(path: "my-stream", broadcast: broadcast)

try audio.writeFrame(payload: payload, timestampUs: 0)
try audio.writeFrame(payload: payload, timestampUs: 20_000)
try audio.finish()
try broadcast.finish()
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
