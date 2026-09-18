# [S] Kotlin: moq-net Maven artifacts without the codecs

## Goal

Maven Central carries `dev.moq:moq-ffi-net` and `dev.moq:moq-net` beside
`dev.moq:moq-ffi` and `dev.moq:moq`. The `-net` pair bundles jniLibs built
with `--no-default-features`, generated bindings without the video and audio
classes, and the `kt/moq` wrapper without `Video.kt` and its audio
counterpart. An Android app that depends on `moq-net` never links a codec.

## Plan

`kt/scripts/generate.sh` takes a variant: `build.sh` flags, output module
directory, and artifact id. Gradle gets two library modules per layer
(`moq-ffi-net`, `moq-net`) whose source sets point at the shared directories
with the codec files excluded via `sourceSets` `exclude`, so nothing is copied.
`moq-net` depends on `moq-ffi-net`, never `moq-ffi`. The exclusion list is
every file that names a codec type: `Video.kt`, the audio counterpart, and
the codec halves of `Aliases.kt` and `Flows.kt`, which split into a shared
file and a codec file first.
The generated Kotlin package name stays `dev.moq.ffi` in both artifacts; an
app depends on one pair, never both, and the README says so.

`release-kt-ffi.yml` (through the reusable release-ffi workflow) builds each
ABI twice and stages both jniLibs sets. The wrapper publishes separately from
`release-kt-lib.yml`, so that workflow gains the same `maven-exists` gate and
publish task for `dev.moq:moq-net`, with its POM range pointing at
`moq-ffi-net`. `SmokeTest` gains a `moq-net` run that
publishes and subscribes a raw track on the JVM target.

Public API: additive. Existing artifact ids, package names, and classes are
untouched.

## Required

- [Measure](/quest/m2/slim-bindings/measure.md) - the go or no-go for the line
- [FFI release workflow](/quest/m0/tooling/release-ffi.md) - the matrix doubles in one file, not five
