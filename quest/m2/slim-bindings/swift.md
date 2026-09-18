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
generated `Aliases.swift` names codec types, so it is split the same way.

`release-swift-ffi.yml` (through the reusable release-ffi workflow) builds
each target twice and stages two xcframework zips; `Package.swift.template`
gets both `binaryTarget` entries with their checksums. `SmokeTests` gains a
`MoqNet` case that publishes and subscribes a raw track.

Public API: additive. No existing product, target, or symbol changes.

## Required

- [Measure](/quest/m2/slim-bindings/measure.md) - the go or no-go for the line
- [FFI release workflow](/quest/m0/tooling/release-ffi.md) - the matrix doubles in one file, not five
