# [S] Binding auth polish

## Goal

Three gaps left by [moq-dev/moq#6137](https://github.com/moq-dev/moq/pull/6137)
close: OBS reconnects with the token the user last presented, moq-ffi wraps
one auth handle type instead of two, and Swift exposes grants as an
`AsyncSequence`.

## Plan

Decided 2026-10-21 with the maintainer: one m1 quest.

- **OBS reconnect.** A reconnect dials the URL's original token, not the one
  presented mid-stream through `MoQOutput::PresentToken`. Once tokens can
  ride SETUP, the presented token replaces it on reconnect.
- **FFI dispatch.** `rs/moq-ffi/src/auth.rs` wraps both
  `moq_net::auth::Handle` (server sessions) and moq-tokio's
  `connection::auth::Handle` (client sessions). With one handle spanning a
  connection, collapse the enum. #6137 decided whichever of it and
  [moq-dev/moq#6109](https://github.com/moq-dev/moq/pull/6109) lands second
  does this; skip it if that already happened.
- **Swift.** Grants arrive as an `AsyncSequence`, once the generated types
  are `Sendable`. If they still are not, say why in the PR rather than
  forcing it.

Public API: Swift additive. Wire: none.

## Required

- [Auth outlives its session](/quest/m1/auth/issuer.md) - one handle per connection, which the enum collapse needs
- [Token in band](/quest/m1/auth/token-in-band.md) - tokens that ride SETUP, which the OBS reconnect needs
