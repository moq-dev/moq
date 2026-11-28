# [M] Encrypted rooms

## Goal

`@moq/room` runs a room over opaque encrypted broadcast paths: participants
discover each other, publish and watch with a shared room credential, and
the relay sees no participant name, kind, or room metadata in plaintext.

## Plan

Decided 2026-11-26 (maintainer, /quest-plan of moq-dev/hang.live's
follow-ups): an encrypted room mode belongs upstream, as a child of this
epic. Found by [moq-dev/hang.live#66](https://github.com/moq-dev/hang.live/pull/66),
whose private rooms wait on it.

`@moq/room` reads identity and kind from plaintext path segments
(`js/room/src/path.ts`), which opaque paths cannot carry.

- Derive each participant's opaque path from the room credential and its
  semantic name, as [encrypted browser components](/quest/m5/e2ee/browser.md)
  do for one broadcast, and recover identity and kind after decryption
  instead of from the path.
- Carry the room's own tracks (location, chat, user fields) through the same
  protected naming, so no room metadata is relay-visible.
- Leave sender authenticity among holders of the same credential out of
  scope, as the epic does.
- Cover discovery, join, leave and a restarted publisher under a new epoch in
  tests, with a check that relay-visible paths and payloads carry no
  plaintext names.

## Required

- [Encrypted browser components](/quest/m5/e2ee/browser.md) - encrypted
  publish and watch that rooms are built on
