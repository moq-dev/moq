# Moq (Swift)

An ergonomic Swift wrapper around the [moq-ffi](../rs/moq-ffi) UniFFI bindings for [Media over QUIC](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/).

## Install

Add the package via Swift Package Manager pointing at the [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift) mirror repo:

```swift
.package(url: "https://github.com/moq-dev/moq-swift", from: "0.2.0"),
```

The mirror repo is updated by [`swift/scripts/publish.sh`](scripts/publish.sh) on every `moq-ffi-v*` tag in the main repo. It carries a bare-semver tag (e.g. `0.2.11`) that SPM can resolve, and a `Package.swift` whose `MoqFFI` binary target points at the `MoqFFI.xcframework.zip` attached to the matching GitHub Release here.

## Quick start

```swift
import Moq

let client = MoqClient()
let cs = try await client.connect(url: "https://relay.example.com")

// cs.publisher() and cs.consumer() are always populated: by whatever
// origin you wired via setPublish / setConsume before connect, or by a
// fresh auto-created one for any side you didn't set. The duplex no-config
// path (the typical client) shares one origin between both.
let consumer = cs.consumer()
let announced = try consumer.announced(prefix: "demos/")
for try await announcement in announced {
    print("got broadcast \(announcement.path())")

    let catalog = try announcement.broadcast().subscribeCatalog()
    for try await update in catalog.updates {
        print("catalog: \(update)")
    }
}

cs.shutdown()
```

Cancelling the surrounding Swift `Task` propagates through to the underlying `cancel()` calls on each consumer. `cs.shutdown()` is an alias for `cancel(code: 0)` that documents the convention that code 0 means "no error".

To publish a broadcast through the auto-created origin:

```swift
let pub = cs.publisher()
let broadcast = try MoqBroadcastProducer()
// ... configure tracks on broadcast ...
try pub.announce(path: "my-stream", broadcast: broadcast)
```

A note on enum casing: UniFFI keeps Rust's casing for error variants (every `MoqError` case is PascalCase and carries `message: String`, e.g. `MoqError.Closed(message: "...")`), but plain enums round-trip to lowerCamelCase (`MoqAudioFormat.s16`, `MoqAudioCodec.opus`).

## Local development

`swift/scripts/check.sh` builds `moq-ffi` for the host, regenerates the UniFFI Swift bindings, builds a single-slice `MoqFFI.xcframework`, and runs `swift test`. Requires macOS with `xcodebuild` and `swift` on `$PATH`. Invoked by `just check-ffi`; skips cleanly on non-macOS hosts.

The `release-swift.yml` workflow triggers on the same `moq-ffi-v*` tag as the Kotlin and Python releases, so the Swift package version always echoes moq-ffi's.

## Layout

```text
swift/
  Package.swift           Local-dev manifest (path-based MoqFFIBinary; used by check.sh + IDEs)
  Package.swift.template  Released manifest (URL + checksum; substituted by package.sh)
  Sources/
    Moq/                  Ergonomic shim (Moq.swift, AsyncSequences.swift, Errors.swift, Session.swift)
    MoqFFI/               UniFFI-generated swift (populated by check.sh/package.sh, gitignored)
  Tests/MoqTests/         Smoke tests
  scripts/                check.sh, package.sh, verify.sh, publish.sh
```

The two manifests are intentionally separate: the in-repo `Package.swift` is what SPM and Xcode see during local development, while `Package.swift.template` is the source-of-truth for what ships to the mirror. Edit the template when changing the released manifest; never copy the dev-mode form into the release path.

## Publishing to SPM

Every `moq-ffi-v*` tag attaches `MoqFFI.xcframework.zip` to the GitHub Release here and mirrors a self-contained Swift package to [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift) on a bare-semver tag that SPM can resolve. No extra configuration: the `moq-bot` GitHub App (already used by `release-rs.yml`) has `contents: write` on the mirror, and `release-swift.yml` mints a fresh installation token per run.

Before the push, a `verify` job builds a throwaway SPM consumer against the staged package (via [`scripts/verify.sh`](scripts/verify.sh)) and runs `swift package resolve` + `swift build`. That resolves the binary target against the just-uploaded `MoqFFI.xcframework.zip` and verifies its SHA-256 checksum, so a manifest SPM cannot resolve never reaches the mirror.

The `publish` job ("Publish to Swift Package mirror") runs `publish.sh`, which clones the mirror, replaces its tree with the staged package, commits, tags with `${VERSION}` (bare semver), and pushes.

To dry-run locally, run `BUILD_VERSION=<v> ./swift/scripts/publish.sh --dry-run` against a staged tarball. Dry-run uses an anonymous clone (so the mirror must be public, or you must export `SWIFT_MIRROR_TOKEN` to authenticate), stages the diff, and skips the commit and push.

No Apple Developer account or App Store Connect setup needed.
