---
title: Swift Libraries
description: Swift Package for Media over QUIC on Apple platforms
---

# Swift Libraries

The Swift bindings expose [Media over QUIC](/) to iOS, iPadOS, macOS, and the iOS Simulator. Built on the same Rust core ([moq-ffi](https://crates.io/crates/moq-ffi)) as the Python and Kotlin packages, wrapped with an idiomatic async/await API.

## Packages

Two Swift packages, split so the ergonomic API can evolve on its own cadence:

### Moq

[moq-dev/moq-swift](https://github.com/moq-dev/moq-swift)

The package you want. A Swift-native wrapper: de-prefixed types (`Client`, `Session`, `BroadcastProducer`), `AsyncSequence` conformance on every consumer, structured-concurrency cancellation, and `Sendable` handles. The raw `MoqFFI` types stay behind it.

**Features:**

- iOS 15+, iPadOS 15+, macOS 12+
- Universal binary for Apple Silicon and Intel Macs
- iOS device + iOS Simulator slices (arm64 and x86\_64)
- Cancellation through Swift `Task` propagates to native consumers
- Versioned independently of the Rust crates; floats to the latest compatible `MoqFFI` patch

[Learn more](/lib/swift/moq)

### MoqFFI

[moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi)

The raw UniFFI bindings (the `Moq`-prefixed classes) plus the prebuilt `MoqFFI.xcframework`, tracking the [`moq-ffi`](https://crates.io/crates/moq-ffi) Rust crate one-to-one. `Moq` pulls this in for you; depend on it directly only if you need the unwrapped API.

## Installation

Add the `Moq` wrapper; SPM resolves `MoqFFI` (and its XCFramework) transitively:

```swift
dependencies: [
    .package(url: "https://github.com/moq-dev/moq-swift", from: "0.3.0"),
],
targets: [
    .target(
        name: "MyApp",
        dependencies: [
            .product(name: "Moq", package: "moq-swift"),
        ],
    ),
]
```

Or in Xcode: File → Add Package Dependencies → enter the URL.

The transitive `MoqFFI.xcframework` is attached to the matching [`moq-ffi-v*` release](https://github.com/moq-dev/moq/releases) on the source repo. SPM downloads it transparently; no manual asset handling required.

## Quickstart

```swift
import Moq

let client = Client()
let session = try await client.connect(to: "https://relay.example.com")

// session.publisher and session.consumer are always populated: by whatever
// origin you wired via setPublish / setConsume before connect, or by a fresh
// auto-created one. The duplex no-config path (the typical client) shares one
// origin between both sides.
let announced = try session.consumer.announced(prefix: "demos/")
for try await announcement in announced {
    print("got broadcast \(announcement.path)")

    let catalog = try announcement.broadcast.subscribeCatalog()
    for try await update in catalog {
        print("catalog: \(update)")
    }
}

session.shutdown()
```

Cancelling the surrounding Swift `Task` propagates through to the underlying `cancel()` calls on each consumer.

## Source and issues

- Source: [swift/](https://github.com/moq-dev/moq/tree/main/swift) (in the monorepo)
- Mirrors (what SPM resolves): [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift) (wrapper), [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi) (raw bindings)
- README: [swift/README.md](https://github.com/moq-dev/moq/blob/main/swift/README.md)
