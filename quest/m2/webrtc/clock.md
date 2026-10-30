# [XS] The WebRTC stack takes moq-clock time

## Goal

The vendored str0m and dimpl take `moq_clock::Instant` instead of `std::time::Instant`, so moq-webrtc drops its `into_std()` conversions and sim tests drive the stack's timers directly.

## Plan

Decided 2026-10-30 with the maintainer (planning interview). Every call into str0m converts with `into_std()` today (see the third-party clocks note in [time-test-sweep](/quest/m2/time-test-sweep.md)). Change the vendored modules' time type, delete the conversions, and move any moq-webrtc test that pauses tokio time onto the sim.

Public API: none. Wire: none.

## Required

- [Vendor and trim the stack](/quest/m2/webrtc/vendor.md) - the modules whose time type changes
