# [M] Metadata-only subscriptions over moq-transport

## Goal

A moq-transport hop between two of our nodes carries a metadata-only
subscription as headers, instead of the payload subscription a relay
otherwise opens over moq-transport and drops the payload of
([moq-dev/moq#9758](https://github.com/moq-dev/moq/pull/9758)). A peer that does not
declare the extension keeps getting that fallback, and nothing refuses.

## Plan

Decided 2026-11-25 (maintainer, /quest-run, on
[moq-dev/moq#9516](https://github.com/moq-dev/moq/pull/9516#issuecomment-7168011073)):
✅ define a moq-transport extension for metadata-only subscriptions (rejected:
refusing the mode over moq-transport; no moq-transport counterpart). It is
its own quest so the lite-07 wire and its serving land without waiting on it.

Recommended, as the low-effort compatibility layer AGENTS.md makes
moq-transport:

- The spec text is a new standalone draft, `drafts/draft-lcurley-moq-meta.md`
  ("MoQ Metadata Extension"), beside MoQ Hidden and MoQ Solicit, landing with
  the code since `drafts/` holds only drafts the code implements.
- It mirrors lite-07's rules by reference rather than restating them: a
  Setup Option declares it, as MoQ Hidden's does, and a SUBSCRIBE parameter
  sent only to a peer that declared it asks for metadata, fixed for the
  subscription's life. Each object then carries its Object Properties,
  Timestamp included, and no payload, plus a property with its declared
  size, so a real empty object stays distinguishable.
- No public API: `moq-net` and `js/net` pick the extension or the
  payload-and-drop fallback from the peer's SETUP when they route the
  subscription, and serve the parameter as a publisher. Both sides in Rust
  and JS.

Tests: a relay over a moq-transport hop whose peer declares the extension
carries no payload across it; one whose peer does not falls back to payload.
Add both to `just test interop --all`.

Public API: none. Wire: a new moq-transport extension, opt-in by Setup
Option.
