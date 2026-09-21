# [M] Verify native and mobile support before closing #700

## Goal

The selected mobile media architecture has a documented, usable capture,
encode, subscribe, decode, and render path on iOS and Android. The next binding
work and deferred mobile phases are complete before #700 closes.

## Plan

This quest owns the cross-phase completion proof, not another implementation.
Use the chosen Rust or platform-owned capture path and the existing binding
APIs. Record the supported path, limitations, and reproducible device results;
wire repeatable coverage into CI and identify the hardware evidence separately.
Update the native/mobile getting-started docs with the working path.

The ownership decision may replace the Rust capture quests with platform-owned
work. In that case, update this quest's Required links to the replacement
implementation and proof quests before removing the abandoned blockers.
Abandoning a backend is not evidence that native/mobile support is complete.
Do not close #700 merely because its next subset or a design decision finished.

## Required

- [Mobile bindings](/quest/next/mobile/README.md) - the independent FFI consumer and Dart iOS asset proof
- [Dart codec parity](/quest/next/dart-codecs.md) - codec-enabled artifacts and Dart video consumer integration
- [Mobile ownership](/quest/future/mobile-ownership.md) - select and scope the mobile media architecture
- [iOS capture](/quest/future/mobile-capture-ios.md) - deliver the selected iOS capture path
- [Android capture](/quest/future/mobile-capture-android.md) - deliver the selected Android capture and codec path

## Closes

- [#700](https://github.com/moq-dev/moq/issues/700) - native and mobile support, including the deferred phases
