# [M] Dart codec parity

## Goal

A Dart application can capture and encode, like a Swift or Kotlin one. Today
the Dart libraries are built `--no-default-features`, so they carry no `audio`
or `video`: no `MoqAudioProducer`, no `MoqVideoProducer`, and no encoder or
decoder anywhere.

## Plan

Catalog and container types are present, so an application can carry
already-encoded frames and nothing is broken. It is the one binding that
cannot originate media, which makes it the odd one out rather than the lean
one.

There is no cross-compilation reason for the difference, which is what makes
this worth closing rather than documenting: `release-kt-ffi.yml` already
builds the same Android and desktop targets with default features, and
`release-swift-ffi.yml` the same iOS targets. The cost is real but known:
`audio` and `video` pull roughly forty crates plus openh264's vendored C++,
which is why Dart shipped without them first.

Build the release artifacts with defaults, check the size and build-time cost
against the Kotlin and Swift artifacts that already pay it, and update
`/doc/lib/dart`, which currently documents the omission.

Deployment floors stay as they are (Android API 24, iOS 16); a codec addition
that raises either is a separate decision.

## Required

- [#3100](/quest/m3/3100-dart-flutter-bindings-via-uniffi.md) - there are no Dart libraries to build with codecs yet
