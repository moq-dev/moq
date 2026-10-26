# [M] Binding auth polish

## Goal

Two gaps left by [moq-dev/moq#6137](https://github.com/moq-dev/moq/pull/6137)
close: OBS reconnects with the token the user last presented, and Swift
exposes grants as an `AsyncSequence`.

## Plan

Decided 2026-10-21 with the maintainer: one m1 quest.

- **OBS reconnect.** A reconnect dials the URL's original token, not the one
  presented mid-stream through `MoQOutput::PresentToken`. The dock passes its
  token through the client's tokens instead of the URL, since a URL's `jwt`
  overrides them, and `PresentToken` replaces it with `Tokens::set()` rather
  than `Auth::add()`, so one owner presents and replays each token.
- **Swift.** Grants arrive as an `AsyncSequence`. UniFFI 0.32 emits
  `Sendable` for the grant record on Swift 6, but its async bridge does not
  cancel a pending Rust future when the Swift task is cancelled, so the
  sequence needs a native cancel like the other consumer wrappers.

Decided 2026-10-25 with the maintainer:

- ✅ **Replacement feedback.** Keep the dock's precise refusal: replacement
  outcomes become observable on the existing token controller
  (`connect::Tokens`), mirrored through moq-ffi and every wrapper.
- ✅ **Grant iteration.** A cancellable grant-watch consumer comes before the
  Swift sequence, so cancelling iteration leaves no native wait pending.
  Whether it replaces `grantChanged()` is decided on the merits.

Open: the API shape of both, proposed in
[moq-dev/moq#6834](https://github.com/moq-dev/moq/pull/6834). Recommended:

- `Tokens::set()` returns a replacement handle whose `answered()` resolves
  once the client's sessions accept the new tokens, in band or as the next
  dial's connection credential, or fails with the first refusal, `Unsupported`
  on a session without AUTH, or `Cancelled` once a later `set()` supersedes it.
  moq-ffi: `MoqTokens.set()` returns `MoqTokenReplacement` with `answered()`
  and `cancel()`, which stops the wait without undoing the set.
- `MoqAuth.grants()` returns a `MoqGrantConsumer` with `grant()`,
  `grant_changed()`, and `cancel()`, mirroring `MoqBandwidthConsumer` and the
  Kotlin and Dart `grants()` streams. It replaces `MoqAuth.grant_changed()`,
  so a shared `MoqAuth` no longer needs one handle per watcher.

Public API: breaking in moq-ffi and every wrapper (`MoqTokens.set` returns a
handle, `grant_changed` moves to the consumer), additive in moq-tokio's
`Tokens::set` return value. Wire: none.
