# [XS] The WebRTC stack takes kio clock time

## Goal

The vendored str0m and dimpl take `kio::clock::Instant` instead of `std::time::Instant`, so moq-webrtc drops its `into_std()` conversions and sim tests drive the stack's timers directly.

## Plan

Decided 2026-10-30 with the maintainer (planning interview). Every call into str0m converts with `Instant::into_std` today, the way the workspace handles any third-party API that takes `std::time::Instant`. Change the vendored modules' time type, delete the conversions, and move any moq-webrtc test that pauses tokio time onto the sim.

Public API: none. Wire: none.

## Required

- [Vendor and trim the stack](/quest/m2/webrtc/vendor.md) - the modules whose time type changes
