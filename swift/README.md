# Moq (Swift)

A Swift-native wrapper around the [moq-ffi](../rs/moq-ffi) UniFFI bindings for [Media over QUIC](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/): de-prefixed types, `AsyncSequence` streams, throwing initializers, and `Sendable` handles.

## Two packages

The Swift integration ships as two SPM packages, each mirrored to its own repo:

| Package | Mirror | Versioning |
|---|---|---|
| `Moq` (this wrapper) | [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift) | independent (`swift/VERSION`) |
| `MoqFFI` (raw bindings + XCFramework) | [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi) | lockstep with the `moq-ffi` crate |

`Moq` depends on `MoqFFI` at `.upToNextMinor`, so a `moq-ffi` patch flows to consumers with no wrapper release. The split mirrors what `py/` does with `moq-rs` (wrapper) and `moq-ffi` (bindings).

## Install

```swift
.package(url: "https://github.com/moq-dev/moq-swift", from: "0.3.0"),
```

SPM resolves `MoqFFI` (and its prebuilt `MoqFFI.xcframework`, attached to the matching `moq-ffi-v*` GitHub Release) transitively. You only depend on `moq-swift`.

## Quick start

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

To publish through the auto-created origin:

```swift
let broadcast = try BroadcastProducer()
// ... configure tracks on broadcast ...
try session.publisher.announce(path: "my-stream", broadcast: broadcast)
```

Cancelling the surrounding Swift `Task` propagates through to the underlying `cancel()` calls on each consumer. `session.shutdown()` is an alias for `cancel(code: 0)` (code 0 means "no error").

A note on enum casing: `MoqError` keeps Rust's PascalCase variants, each carrying `message: String` (e.g. `MoqError.Closed(message: "...")`); plain enums round-trip to lowerCamelCase (`AudioFormat.s16`, `AudioCodec.opus`).

## API shape

The wrapper fully wraps every stateful handle (`Client`, `Session`, `BroadcastProducer`, `TrackConsumer`, …) and re-exports the plain data records/enums under de-prefixed names via typealias (`Frame`, `Catalog`, `Audio`, `Container`, …). Because the records are typealiased, new fields on the `moq-ffi` side flow through automatically; only new FFI *methods* need a matching wrapper method.

Every consumer conforms to `AsyncSequence`, so `for try await x in consumer` works directly. `TrackConsumer` iterates groups in sequence order; use its `groupsAsArrived` property for arrival order.

## Local development

`swift/scripts/check.sh` builds `moq-ffi` for the host, regenerates the UniFFI Swift bindings, builds a single-slice `MoqFFI.xcframework`, and runs `swift test`. Requires macOS with `xcodebuild` and `swift` on `$PATH`. Invoked by `just check-ffi`; skips cleanly on non-macOS hosts.

Local development uses one **monolithic** `Package.swift` containing both the `Moq` and `MoqFFI` targets plus the path-based XCFramework, so `swift test` and Xcode work against a single package. The split into two packages exists only in the released artifacts, assembled from the two templates below at release time. Because the FFI module is named `MoqFFI` in both layouts, the wrapper sources (`import MoqFFI`) compile identically either way.

## Layout

```text
swift/
  VERSION                     Wrapper's independent version (bump by hand to release)
  Package.swift               Monolithic local-dev manifest (both targets, path-based; used by check.sh + IDEs)
  Package.swift.template      Released WRAPPER manifest (Moq + dep on moq-swift-ffi; REPLACE_FFI_VERSION)
  ffi/Package.swift.template  Released FFI manifest (MoqFFI + binaryTarget; REPLACE_URL/REPLACE_CHECKSUM)
  Sources/
    Moq/                      Ergonomic wrapper (Client, Server, Origin, Broadcast, Track, Media, Audio, …)
    MoqFFI/                   UniFFI-generated swift (populated by check.sh/package-ffi.sh, gitignored)
  Tests/MoqTests/             Smoke tests
  scripts/                    check.sh, package{,-ffi}.sh, verify{,-ffi}.sh, publish{,-ffi}.sh
```

Edit the templates when changing a released manifest; never copy the monolithic dev-mode form into the release path.

## Publishing to SPM

Two workflows, mirroring the two packages:

- **`release-swift-ffi.yml`** fires on each `moq-ffi-v*` tag (pushed by release-plz). It builds the per-target libs + bindings, assembles the `MoqFFI` package via `package-ffi.sh`, attaches `MoqFFI.xcframework.zip` to the `moq-ffi-v*` GitHub Release, verifies the staged package resolves (`verify-ffi.sh`), and mirrors it to [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi) on a bare-semver tag (`publish-ffi.sh`).
- **`release-swift.yml`** fires on push to `main`/`dev` when `swift/VERSION` (or the wrapper sources) change. It reads `swift/VERSION`, checks whether that tag already exists on the mirror (the release gate, the same model release-plz uses for crates), assembles the wrapper via `package.sh` (substituting the `moq-ffi` pin from `rs/moq-ffi/Cargo.toml`), verifies it resolves against the published `MoqFFI` (`verify.sh`), and publishes to [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift) only when the version is new (`publish.sh`).

Both `verify` jobs build a throwaway SPM consumer against the staged package before any mirror push, so a manifest SPM cannot resolve never reaches consumers. The `moq-bot` GitHub App mints a fresh installation token per run, scoped to the relevant mirror.

To release a new wrapper version: bump `swift/VERSION` in a PR. On merge, `release-swift.yml` publishes it.

To dry-run a publish locally against a staged tarball:

```bash
BUILD_VERSION=<v> ./swift/scripts/publish.sh --dry-run        # wrapper -> moq-swift
BUILD_VERSION=<v> ./swift/scripts/publish-ffi.sh --dry-run    # bindings -> moq-swift-ffi
```

No Apple Developer account or App Store Connect setup needed.
