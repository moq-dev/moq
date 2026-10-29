# [S] moq-audio captures and encodes while enabled

## Goal

`moq-audio`'s capture supervisor keeps the microphone open and encodes while
enabled, whatever the demand. An on-demand option restores today's behavior.

## Plan

Decided 2026-10-28 (maintainer); see the [epic](/quest/m1/eager-encode/README.md).

- The gate: the supervisor at `rs/moq-audio/src/encode/capture.rs:1035-1142`
  (idle until used, closes the device when unused), the demand trait at
  `:755-770`, and `Control::new` (`:455-485`) and `publish_capture` (`:675`).
- Update `lib.rs:22-25`, `aec.rs:179` and `doc/lib/rs/moq-audio.md:24`.

Tests: with no subscriber, the capture publisher writes groups. The opt-in
keeps the device closed until demand arrives.

Public API: default behavior change, plus an additive option. Wire: none.
