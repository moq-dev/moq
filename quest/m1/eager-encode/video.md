# [M] moq-video captures and encodes while enabled

## Goal

`moq-video`'s capture publisher keeps the camera open and encodes while
enabled, whatever the demand. An on-demand option on the capture config
restores today's close-between-viewers behavior.

## Plan

Decided 2026-10-28 (maintainer); see the [epic](/quest/m1/eager-encode/README.md).

- The gate: `rs/moq-video/src/encode/producer.rs:3-13` (the module doc's
  "strictly on demand"), `:525` (`publish_capture`) and `:660-700`
  (`until_wanted`/`wait_capture`).
- The demand-driven PipeWire reopen (`capture/pipewire.rs:14,157,304,866`)
  serves only the opt-in.
- Update the notes at `capture/mod.rs:116`, `lib.rs:29` and `encode/mod.rs:19`,
  and `doc/lib/rs/moq-video.md:30`.

Tests: with no subscriber, the capture publisher writes groups. The opt-in
keeps the camera closed until demand arrives.

Public API: default behavior change, plus an additive option. Wire: none.
