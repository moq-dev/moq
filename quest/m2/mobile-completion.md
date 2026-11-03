# [M] Verify native and mobile support end to end

## Goal

The selected mobile media architecture has a documented, usable capture,
encode, subscribe, decode, and render path on iOS and Android, and the
deferred mobile phases are complete.

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

Do not finish this quest merely because its next subset or a design decision
finished.

Promoted from m2 on 2026-10-09 by maintainer priority, with both capture
quests. Deferred back to m2 in #5427 (`8f0dfa760`) with those capture
quests; mobile completion does not gate the next breaking cut.

## Required

- [Dart on iOS](/quest/m2/dart-ios.md) - the Dart iOS asset proof
- [iOS capture](/quest/m2/mobile-capture-ios.md) - deliver the selected iOS capture path
- [Android capture](/quest/m2/mobile-capture-android.md) - deliver the selected Android capture and codec path
