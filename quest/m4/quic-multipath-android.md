# [M] An Android client opens a path per network

## Goal

An Android app using the Kotlin binding keeps one MoQ session across Wi-Fi
and cellular: it hands the session each network it holds, the session opens
a path on it with a role, and a session on Wi-Fi with cellular as backup
keeps playing or publishing when Wi-Fi drops, with no redial.

## Plan

Decided 2026-11-26 (`/quest-plan` via kixel-ai/kixel.ai): ✅ Android and iOS
follow Linux at medium priority, wanted more for failover than for
duplication, so the default this documents is Wi-Fi available and cellular
backup (rejected: every platform in the first work).

Recommended:

- Android routes per network, and an app selects one by binding a socket to
  it, so this needs a socket per path, bound with the NDK's
  `android_setsocknetwork` to a network handle the app obtained from
  `ConnectivityManager`. The
  [protocol quest](/quest/m3/quic-multipath/core.md)'s opaque local handle
  exists for this.
- The Kotlin API takes a network and a role, mirroring the Rust session
  handle's names. `AGENTS.md` asks that a binding feature reach every
  language together, so add the same calls to the other bindings as a
  refusal on platforms that cannot open a path, and to Swift with
  [iOS](/quest/m4/quic-multipath-ios.md).
- The app owns which networks to request and when; the library opens paths
  only on what it is handed.

Test on an emulator or device with two networks: the second path
validates, and disabling Wi-Fi mid-session leaves the session open with a
bounded gap. Report the gap.

Public API: additive in moq-ffi and the Kotlin package. Wire: none.

## Required

- [moq-tokio opens extra paths on Linux, and relays accept them](/quest/m3/quic-multipath/paths-linux.md) - the session handle and a relay that accepts paths

## Related

- [QUIC multipath](/quest/m3/quic-multipath/README.md) - the decisions this builds on
