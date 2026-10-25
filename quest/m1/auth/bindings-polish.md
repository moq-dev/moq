# [S] Binding auth polish

## Goal

Two gaps left by [moq-dev/moq#6137](https://github.com/moq-dev/moq/pull/6137)
close: OBS reconnects with the token the user last presented, and Swift
exposes grants as an `AsyncSequence`.

## Plan

Decided 2026-10-21 with the maintainer: one m1 quest.

- **OBS reconnect.** A reconnect dials the URL's original token, not the one
  presented mid-stream through `MoQOutput::PresentToken`. Once tokens can
  ride SETUP, the presented token replaces it on reconnect.
- **Swift.** Grants arrive as an `AsyncSequence`, once the generated types
  are `Sendable`. If they still are not, say why in the PR rather than
  forcing it.

Public API: Swift additive. Wire: none.

## Required

- [Token in band](/quest/m1/auth/token-in-band.md) - tokens that ride SETUP, which the OBS reconnect needs
