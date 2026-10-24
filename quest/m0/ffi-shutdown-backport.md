# [S] Backport draining shutdown and the qmux stream limit to release

## Goal

The `release` line publishes a non-breaking moq-ffi 0.4.x (wheel, Swift, and
Kotlin) and moq-tokio whose sessions can drain before closing and whose qmux
sessions accept the QUIC stream limit:

- An async `MoqSession.drain()` drains finished tracks within one second
  and returns any delivery error, as the earlier async shutdown implementation did
  (7bbac3272, with 03fb0d1c1, f9de31167, and #5558's explicit Lite 07 test).
  Explicit `moq-lite-07-wip` waits for the peer to receive the tail; older
  Lite versions, including the default lite-06, and IETF wait only for
  transport ACKs. The sync `shutdown()` stays `cancel(0)`.
- qmux sessions get the QUIC stream limit (#5609) instead of the 100-stream
  default.

Check: release-plz publishes the release commits, and the newest moq-ffi on
PyPI, the Swift package, and Maven Central contains both. Swift and Kotlin
publish from their own tag workflows, so check each. The newest hand-written
wrappers (`moq-rs`, the Swift and Kotlin libraries, and the other wrappers
that gain `drain()`) expose it too.

## Plan

Decided 2026-10-20 (maintainer): additive `drain()` on `release` rather than
backporting async `shutdown()` as-is. Why: making `shutdown()` async breaks
the 0.4.x Python, Swift, and Kotlin bindings. `main` requests the drain with synchronous `shutdown()` and reports its result
through `closed()`; it gains no separate `drain()`. Callers replace the release
line's awaited `drain()` with `shutdown()` followed by awaiting `closed()` at the
next breaking cut.

Decided 2026-10-20 (maintainer): keep #5609 in the backport, on a qmux
0.5.3 published from web-transport's `qmux-v0.5.x`. Why: `release` locks
qmux 0.5.2, which lacks the builder session config #5609 uses, and
moq-dev/web-transport#427 already carries that config for 0.5.

Follow the backport workflow. #5609 moves `release` to qmux 0.5.3 and takes
its builder session config instead of 0.6.3's. The drain backport
adapts 7bbac3272's session close to the `release` line's moq-net and keeps
its tests, renamed to `drain()`. Follow the Cross-Package Sync row for
`rs/moq-ffi`: every binding and its docs gain `drain()`, or the PR says why
one has none. Wrapper workflows skip versions already published, so bump
each changed wrapper's human-owned version (CONTRIBUTING's Versions). Leave
`go/wrapper/VERSION` alone: CI derives Go's patch, and `drain()` is
additive. Run each binding's tests and `just test interop --all`. The
qmux credit fix is out of scope: main has it from the in-tree qmux
([moq-dev/moq#6231](https://github.com/moq-dev/moq/pull/6231)).

Public API: additive (`MoqSession.drain()` and its binding wrappers). Wire:
none.
