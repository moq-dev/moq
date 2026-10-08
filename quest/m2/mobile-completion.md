# [M] Verify native and mobile support before closing #700

## Goal

The selected mobile media architecture has a documented, usable capture,
encode, subscribe, decode, and render path on iOS and Android. The next binding
work and deferred mobile phases are complete before #700 closes.

## Plan

This quest owns the cross-phase completion proof, not another implementation.
Use the Rust capture path and the existing binding APIs: Rust owns capture
and codecs on mobile, settled in the 2026-09-30 audit. Record the supported
path, limitations, and reproducible device results; wire repeatable coverage
into CI and identify the hardware evidence separately. Update the
native/mobile getting-started docs with the working path.

Do not close #700 merely because its next subset or a design decision finished.

## Required

- [Dart on iOS](/quest/m1/dart-ios.md) - the Dart iOS asset proof
- [Dart codec parity](/quest/m1/dart-codecs.md) - codec-enabled artifacts and Dart video consumer integration
- [iOS capture](/quest/m2/mobile-capture-ios.md) - deliver the selected iOS capture path
- [Android capture](/quest/m2/mobile-capture-android.md) - deliver the selected Android capture and codec path

## Closes

- [#700](https://github.com/moq-dev/moq/issues/700) - native and mobile support, including the deferred phases
