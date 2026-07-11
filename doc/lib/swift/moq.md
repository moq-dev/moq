---
title: Moq (Swift)
description: Swift Package Manager target for Media over QUIC
---

# Moq

The ergonomic Swift Package Manager target for [Media over QUIC](/).

A Swift-native wrapper over the UniFFI-generated bindings: de-prefixed types, `AsyncSequence` streams, throwing initializers, `Sendable` handles, and Swift-friendly errors. The raw `MoqFFI` types it wraps stay out of your way (data types like `Frame` and `Catalog` are re-exported under de-prefixed names).

## Install

```swift
.package(url: "https://github.com/moq-dev/moq-swift", from: "0.3.0"),
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

Supported platforms: iOS 15+, iPadOS 15+, macOS 12+. The XCFramework ships iOS device (arm64), iOS Simulator (arm64 + x86\_64), and macOS universal slices.

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

A server can reject the connection on auth grounds: `MoqError.Unauthorized` (HTTP 401) or `MoqError.Forbidden` (HTTP 403). These are terminal: retrying without new credentials won't help, so handle them separately from a transient transport failure. Use the `isAuth` helper to catch both:

```swift
do {
    let session = try await client.connect(url: "https://relay.example.com")
} catch let error as MoqError where error.isAuth {
    // Prompt for credentials; don't reconnect.
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

## Publish

```swift
let broadcast = try BroadcastProducer()
let audio = try broadcast.publishMedia(format: "opus", initData: opusInitBytes)

try session.publisher.announce(path: "my-stream", broadcast: broadcast)

try audio.writeFrame(payload, timestampUs: 0)
try audio.writeFrame(payload, timestampUs: 20_000)
try audio.finish()
try broadcast.finish()
```

### Fetching raw groups

Fetch retrieves one group by track name and group sequence without keeping a live subscription:

```swift
let group = try await consumer.fetchGroup(
    name: "events",
    sequence: 42,
    options: FetchGroupOptions(priority: 10)
)
for try await frame in group {
    print(frame)
}
```

A retained group resolves immediately. To serve a group that is not retained, keep a dynamic handler alive on its producer:

```swift
let dynamic = try track.dynamic()

for try await request in dynamic {
    let group = try request.accept()
    try group.writeFrame(loadArchivedFrame(request.sequence))
    try group.finish()
}
```

Call `request.abort(errorCode:)` when the requested group cannot be produced. Fetch is currently a single-group operation and is supported by the moq-lite 05+ FETCH wire path.

### On-demand raw tracks

Use a dynamic broadcast when subscribers should be able to request raw tracks that are not published yet:

```swift
let broadcast = try BroadcastProducer()
let dynamic = try broadcast.dynamic()

try session.publisher.announce(path: "events", broadcast: broadcast)

for try await request in dynamic {
    if try request.name == "alerts" {
        let track = try request.accept()
        try track.writeFrame(Data("ready".utf8))
        try track.finish()
    } else {
        try request.abort(errorCode: 404)
    }
}
```

Each request arrives as a `TrackRequest`; call `accept(info:)` to turn it into a `TrackProducer` (omit `info` for defaults), or `abort(errorCode:)` to reject the subscriber.

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

- Source: [swift/Sources/Moq](https://github.com/moq-dev/moq/tree/main/swift/Sources/Moq)
- Mirror repos: [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift) (wrapper), [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi) (raw bindings)
- The Rust crate this wraps: [moq-net](/lib/rs/crate/moq-net)
