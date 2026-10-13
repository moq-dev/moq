# [M] Verify native and mobile support before closing #700

## Goal

The selected mobile media architecture has a documented, usable capture,
encode, subscribe, decode, and render path on iOS and Android. The next binding
work and deferred mobile phases are complete before #700 closes.

## Plan

This quest owns the cross-phase completion proof, not another implementation.
The mobile SDK is the upstream Swift and Kotlin bindings over moq-ffi, with
Rust owning capture and codecs underneath (Rust capture settled in the
2026-09-30 audit; the binding direction decided by the maintainer
2026-10-09). Prove that path rather than a platform-native capture stack
beside it. Record the supported
path, limitations, and reproducible device results; wire repeatable coverage
into CI and identify the hardware evidence separately. Update the
native/mobile getting-started docs with the working path.

Do not close #700 merely because its next subset or a design decision finished.

Promoted from m2 on 2026-10-09 by maintainer priority, with both capture
quests.

## Required

- [Dart on iOS](/quest/m1/dart-ios.md) - the Dart iOS asset proof
- [Dart codec parity](/quest/m1/dart-codecs.md) - codec-enabled artifacts and Dart video consumer integration
- [iOS capture](/quest/m1/mobile-capture-ios.md) - deliver the selected iOS capture path
- [Android capture](/quest/m1/mobile-capture-android.md) - deliver the selected Android capture and codec path

## Closes

- [#700](https://github.com/moq-dev/moq/issues/700) - native and mobile support, including the deferred phases
