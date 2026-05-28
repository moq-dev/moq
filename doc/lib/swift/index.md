---
title: Swift Libraries
description: Swift Package for Media over QUIC on Apple platforms
---

# Swift Libraries

The Swift bindings expose [Media over QUIC](/) to iOS, iPadOS, macOS, and the iOS Simulator. Built on the same Rust core ([moq-ffi](https://crates.io/crates/moq-ffi)) as the Python and Kotlin packages, wrapped with an idiomatic async/await API.

## Packages

### Moq

A single Swift Package Manager target that wraps the UniFFI bindings with `AsyncSequence` adapters, structured-concurrency-friendly cancellation, and a session helper.

**Features:**

- iOS 15+, iPadOS 15+, macOS 12+
- Universal binary for Apple Silicon and Intel Macs
- iOS device + iOS Simulator slices (arm64 and x86_64)
- Cancellation through Swift `Task` propagates to native consumers

[Learn more](/lib/swift/moq)

## Installation

The package lives in [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift), a mirror repo that SPM resolves with bare-semver tags. Add it to your `Package.swift`:

```swift
dependencies: [
    .package(url: "https://github.com/moq-dev/moq-swift", from: "0.2.0"),
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

The package depends on a prebuilt `MoqFFI.xcframework` attached to the matching [`moq-ffi-v*` release](https://github.com/moq-dev/moq/releases) on the source repo. SPM downloads it transparently; no manual asset handling required.

## Quickstart

```swift
import Moq

// Wire an origin as both publish source and consume sink. Set just one
// side for a subscribe-only or publish-only client.
let origin = MoqOriginProducer()
let client = MoqClient()
client.setPublish(origin: origin)
client.setConsume(origin: origin)

let session = try await client.connect(url: "https://relay.example.com")

let consumer = origin.consume()
let announced = try consumer.announced(prefix: "demos/")
for try await announcement in announced.announcements {
    print("got broadcast \(announcement.path())")

    let catalog = try announcement.broadcast().subscribeCatalog()
    for try await update in catalog.updates {
        print("catalog: \(update)")
    }
}

session.shutdown()
```

Cancelling the surrounding Swift `Task` propagates through to the underlying `cancel()` calls on each consumer.

## Source and issues

- Source: [swift/](https://github.com/moq-dev/moq/tree/main/swift) (in the monorepo)
- Mirror (what SPM resolves): [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift)
- README: [swift/README.md](https://github.com/moq-dev/moq/blob/main/swift/README.md)
