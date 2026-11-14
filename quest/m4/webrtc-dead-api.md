# [M] The vendored stack exposes only what moq-webrtc calls

## Goal

Every item in the vendored modules is reached from moq-webrtc or its
tests; dead upstream API is deleted.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8163](https://github.com/moq-dev/moq/pull/8163). It waits on
the clock quest, which reshapes the time seams moq-webrtc calls through, as
the move to rind already did the crypto seams. The vendored tree opens with `#![allow(warnings, reason = "vendored
upstream code")]` (`rs/moq-webrtc/src/vendor/mod.rs`), so first narrow that
allow to let `dead_code` fire, then delete what the report shows unreached,
keeping upstream's tests for what stays.

Public API: none. Wire: none.

## Required

- [The stack takes kio clock time](/quest/m4/webrtc-clock.md) - settles the time seams
