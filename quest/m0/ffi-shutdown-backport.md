# [S] Backport draining shutdown and the qmux stream limit to release

## Goal

The `release` line publishes a non-breaking moq-ffi 0.4.x (wheel, Swift, and
Kotlin) and moq-tokio whose sessions can drain before closing and whose qmux
sessions accept the QUIC stream limit:

- An async `MoqSession.drain()` delivers finished tracks within one second
  and returns any delivery error, as `main`'s async `shutdown()` does
  (7bbac3272, with 03fb0d1c1, f9de31167, and #5558's explicit Lite 07 test).
  The sync `shutdown()` stays `cancel(0)`.
- qmux sessions get the QUIC stream limit (#5609) instead of the 100-stream
  default.

Check: release-plz publishes the release commits, and the newest moq-ffi on
PyPI, the Swift package, and Maven Central contains both. Swift and Kotlin
publish from their own tag workflows, so check each.

## Plan

Decided 2026-10-20 (maintainer): additive `drain()` on `release` rather than
backporting async `shutdown()` as-is. Why: making `shutdown()` async breaks
the 0.4.x Python, Swift, and Kotlin bindings. `main` keeps async `shutdown()`
and gains no `drain()`; callers move to it at the next breaking cut.

Follow the backport workflow. #5609 cherry-picks as is. The drain backport
adapts 7bbac3272's session close to the `release` line's moq-net and keeps
its tests, renamed to `drain()`. Follow the Cross-Package Sync row for
`rs/moq-ffi`: every binding and its docs gain `drain()`, or the PR says why
one has none. Run each binding's tests and `just test interop --all`. The
qmux credit fix is out of scope: it arrives with
[qmux on the QUIC stream state machine](/quest/m1/quic-qmux.md).

Public API: additive (`MoqSession.drain()` and its binding wrappers). Wire:
none.

## Related

- [Release builds link](/quest/m0/release-link.md) - the release binaries
  must link for the cut that publishes this
