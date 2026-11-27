# [M] An iOS client opens a path per interface

## Goal

An iOS app using the Swift binding keeps one MoQ session across Wi-Fi and
cellular: the session opens a path on each interface the app names, with a
role, and a session on Wi-Fi with cellular as backup keeps playing or
publishing when Wi-Fi drops, with no redial.

## Plan

Decided 2026-11-26 (`/quest-plan` via kixel-ai/kixel.ai): ✅ Android and iOS
follow Linux at medium priority, wanted more for failover than for
duplication, so the default this documents is Wi-Fi available and cellular
backup (rejected: every platform in the first work).

Recommended:

- Darwin selects an interface with `IP_BOUND_IF` and `IPV6_BOUND_IF` on a
  socket, or per packet through the interface index in `IP_PKTINFO`. Check
  which one iOS honors for an app, and prefer the per-packet form if it
  works, since it keeps one socket as Linux does.
- The Swift API takes an interface and a role, mirroring the Rust session
  handle's names and the Kotlin API from
  [Android](/quest/m4/quic-multipath-android.md).
- The same Darwin code serves macOS; enabling and testing it there is
  [macOS](/quest/m5/quic-multipath-macos.md)'s.
- The app learns which interfaces exist from `NWPathMonitor`; the library
  opens paths only on what it is handed.

Test on a device with Wi-Fi and cellular, since the simulator has no second
interface: the second path validates, and turning Wi-Fi off mid-session
leaves the session open with a bounded gap. Report the gap.

Public API: additive in moq-ffi and the Swift package. Wire: none.

## Hosts

- macos - the Swift package builds and its device tests run from Xcode

## Required

- [moq-tokio opens extra paths on Linux, and relays accept them](/quest/m3/quic-multipath/paths-linux.md) - the session handle and a relay that accepts paths

## Related

- [QUIC multipath](/quest/m3/quic-multipath/README.md) - the decisions this builds on
