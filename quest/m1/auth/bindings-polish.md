# [S] Binding auth polish

## Goal

Two gaps left by [moq-dev/moq#6137](https://github.com/moq-dev/moq/pull/6137)
close: OBS reconnects with the token the user last presented, and Swift
exposes grants as an `AsyncSequence`.

## Plan

Decided 2026-10-21 with the maintainer: one m1 quest.

- **OBS reconnect.** A reconnect dials the URL's original token, not the one
  presented mid-stream through `MoQOutput::PresentToken`. SETUP credentials
  and `Client::tokens().set()` now exist. The dock must pass its token
  separately from the URL: a URL's `jwt` intentionally overrides the
  client's token set. The existing token controller should own replacement
  and replay, rather than presenting the same token through both it and
  `Auth::add()`.
- **Swift.** Grants arrive as an `AsyncSequence`, once the generated types
  are `Sendable`. UniFFI 0.32 emits `Sendable` for the grant record on Swift
  6. Its async bridge does not cancel a pending Rust future when the Swift
  task is cancelled. The other wrapper sequences explicitly cancel their
  native consumer; `MoqAuth` has no operation to release a grant wait.

Choose the remaining API behavior before implementation:

- **Replacement feedback.** `Tokens::set()` reports configuration errors,
  while its asynchronous peer refusal goes only to logs. The dock currently
  shows the precise refusal returned by `Auth::add()`. Recommend making
  replacement outcomes observable through the existing token controller,
  preserving that feedback with one owner of each presented token. The
  smaller alternative is to show that a token is configured for the next
  dial and leave in-band refusal details to logs.
- **Grant iteration cancellation.** Recommend a cancellable grant-watch
  consumer, following the other consumer wrappers, before exposing a Swift
  sequence. Decide its API shape and whether it replaces the existing
  `grantChanged()` watch. A plain conformance over the existing wait would
  leave a cancelled iteration pending until a grant changes or the session
  ends.

Public API: the chosen token feedback and grant-watch APIs may also affect
moq-ffi and its wrappers; report their full impact. Wire: none.
