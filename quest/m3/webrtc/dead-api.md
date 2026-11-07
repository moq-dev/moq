# [M] The vendored stack exposes only what moq-webrtc calls

## Goal

Every item in the vendored modules is reached from moq-webrtc or its
tests; dead upstream API is deleted.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8163](https://github.com/moq-dev/moq/pull/8163). It waits on
the rind and clock quests, since both reshape the seams moq-webrtc calls
through. Start from the compiler's dead-code report with the vendored
modules private, then remove what remains unreached, keeping upstream's
tests for what stays.

Public API: none. Wire: none.

## Required

- [moq-webrtc on rind](/quest/m1/rind/rtc.md) - settles the crypto seams
- [The stack takes kio clock time](/quest/m3/webrtc/clock.md) - settles the time seams
