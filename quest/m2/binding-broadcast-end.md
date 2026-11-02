# [S] Bindings see a broadcast end

## Goal

A moq-ffi binding (Go, Kotlin, Swift, Python, Dart) can tell that a
broadcast it is reading has ended, apart from a refusal, without matching
error types. `MoqBroadcastConsumer` exposes the broadcast closing, as the
Rust consumer does. A publisher's run ending reaches bindings the same way
over moq-transport as over moq-lite: as a protocol error, not
`MoqProtocolKind::Internal`.

## Plan

Decided 2026-10-30 (moq.pro maintainer, planning the follow-ups of
[moq-dev/moq.pro#3169](https://github.com/moq-dev/moq.pro/pull/3169)). That
PR's Go and Kotlin watch samples tell a run's end from a permanent catalog
refusal only by the error kind: a lite run's end arrives as a protocol
reset. Without a way to see the broadcast close, they can print the old
run's catalog once more during a replacement. A moq-transport run's end
arrives as an internal error, so those samples stop instead of following it.

- Add a `closed()` (or equivalent) to `MoqBroadcastConsumer` in
  `rs/moq-ffi/src/consumer.rs`, resolving when the underlying
  `moq_net::broadcast::Consumer` closes. Cover it in each binding's tests.
- Find where a moq-transport publisher's run end becomes
  `SessionError::Internal` / `StreamError::Internal` for a subscriber, and
  give it the same code lite uses, in both directions of a relay. Cover it in
  the session harness for lite-06, lite-07-wip, and moq-transport-22, direct
  and through a relay.
- Adding `closed()` is additive. If the error-kind change is a wire or API
  break, bring it to the maintainer first, as this milestone's Plan says.
- Downstream, moq.pro's Go and Kotlin samples switch to `closed()` with the
  pin bump that brings this in.
