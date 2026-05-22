# Moq (Swift)

An ergonomic Swift wrapper around the [moq-ffi](../rs/moq-ffi) UniFFI bindings for [Media over QUIC](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/).

## Install

Add the package via Swift Package Manager pointing at the [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift) mirror repo:

```swift
.package(url: "https://github.com/moq-dev/moq-swift", from: "0.2.0"),
```

The mirror repo is updated by [`swift/scripts/publish.sh`](scripts/publish.sh) on every `moq-ffi-v*` tag in the main repo. It contains a `Package.swift` whose `MoqFFI` binary target points at the `MoqFFI.xcframework.zip` attached to the matching GitHub Release.

## Quick start

```swift
import Moq

let session = try await Moq.connect(url: "https://relay.example.com")

let consumer = MoqOriginProducer().consume()
let announced = try consumer.announced(prefix: "demos/")
for try await announcement in announced.announcements {
    print("got broadcast \(announcement.path())")

    let catalog = try announcement.broadcast().subscribeCatalog()
    for try await update in catalog.updates {
        print("catalog: \(update)")
    }
}
```

Cancelling the surrounding Swift `Task` propagates through to the underlying `cancel()` calls on each consumer.

## Local development

`swift/scripts/check.sh` builds `moq-ffi` for the host, regenerates the UniFFI Swift bindings, builds a single-slice `MoqFFI.xcframework`, and runs `swift test`. Requires macOS with `xcodebuild` and `swift` on `$PATH`. Invoked by `just check-ffi`; skips cleanly on non-macOS hosts.

The `release-swift.yml` workflow triggers on the same `moq-ffi-v*` tag as the Kotlin and Python releases, so the Swift package version always echoes moq-ffi's.

## Layout

```text
swift/
  Package.swift           Manifest (URL+checksum rewritten by package.sh at release time)
  Sources/
    Moq/                  Ergonomic shim (Moq.swift, AsyncSequences.swift, Errors.swift, Session.swift)
    MoqFFI/               UniFFI-generated swift (populated by check.sh/package.sh, gitignored)
  Tests/MoqTests/         Smoke tests
  scripts/                check.sh, package.sh, publish.sh
```

## Publishing to SPM

Today the GitHub Release attaches `MoqFFI.xcframework.zip` + a `moq-ffi-${VERSION}-swift.tar.gz` archive. To enable automatic mirroring to a tagged repo that SPM can resolve:

1. Create empty `moq-dev/moq-swift` on GitHub.
2. Provision a GitHub App or fine-grained PAT with `contents: write` on that repo only.
3. In `moq-dev/moq` repo settings:
   - Add secret `SWIFT_MIRROR_TOKEN` containing the token.
   - Add variable `PUBLISH_SPM=true`.
4. Cut the next `moq-ffi-v*` tag. The `publish-spm` job runs `publish.sh`, which clones the mirror, replaces its tree with the staged package, commits, tags, and pushes.

No Apple Developer account or App Store Connect setup needed.
