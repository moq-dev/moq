# [S] Swift: a MoqNet product without the codecs

## Goal

`swift/Package.swift` offers `Moq` and `MoqNet`. `MoqNet` links a
`MoqFFINet.xcframework` built with `--no-default-features`, its generated
`MoqFFINet` target has no video or audio classes, and the `MoqNet` wrapper
target is the `Moq` sources minus `Video.swift` and `Audio.swift`. A consumer
of `MoqNet` never compiles or links a codec.

## Plan

`swift/scripts/package-ffi.sh` and `check.sh` take a variant: which
`build.sh` flags, which xcframework name, which module name for
`uniffi-bindgen`. The UniFFI namespace is `moq` in both builds; the Swift
module names differ (`MoqFFI`, `MoqFFINet`), which is a bindgen config value,
not a Rust change. SwiftPM targets cannot share a source directory, so the
wrapper split is a `Sources/MoqNet` directory of symlinks into `Sources/Moq`
for the files both share, or a generated copy in the packaging script; pick
whichever `swift build` and Xcode both accept and write the reason down. The
shared files `import MoqFFI` today; in the shared set that becomes
`#if canImport(MoqFFINet) import MoqFFINet #else import MoqFFI #endif`, which
resolves per target since each wrapper depends on exactly one binding. `Aliases.swift` names codec types, and `Broadcast.swift` declares
`subscribeAudio`, `publishAudio`, and `publishVideo` against generated
methods the slim binding lacks (`setVideoProperties` stays shared: it only
writes catalog metadata and is not feature-gated in moq-ffi); those move into codec-only
files (`Broadcast+Media.swift`, a codec half of `Aliases.swift`) that only
the `Moq` target compiles. The rule for the split is mechanical: a shared
file compiles against `MoqFFINet`, so `swift build --product MoqNet` is the
check.

`release-swift-ffi.yml` (through the reusable release-ffi workflow) builds
each target twice and stages two xcframework zips; `Package.swift.template`
gets both `binaryTarget` entries with their checksums. A separate `MoqNetTests`
target depending only on `MoqNet` publishes and subscribes a raw track; it
cannot share `MoqTests`, since linking both xcframeworks into one executable
exports the same UniFFI C symbols twice and would let the full library
satisfy the slim product's calls.

Public API: additive. No existing product, target, or symbol changes.

## Required

- [Measure](/quest/m2/slim-bindings/measure.md) - the go or no-go for the line
- [FFI release workflow](/quest/m0/tooling/release-ffi.md) - the matrix doubles in one file, not five
