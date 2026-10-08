# [S] JS readers hold for a track's pending tail

## Goal

A `@moq/net` reader of a received track waits for missing groups below a
declared end until the tail settles, as Rust readers do once #4225 lands,
instead of ending at the end with groups missing (`doc/lib/js/net.md`).

## Plan

Mirror the Rust hold from [Track tail interop](/quest/m1/track-tail-interop.md)
(#4225) once it merges, and test it against the Rust behavior with mocked
time. Split from [JS session parity](/quest/m1/js-session-parity.md) on
2026-10-08 so the caps work stays ready.

Public API: none. Wire: none.

## Required

- [Track tail interop](/quest/m1/track-tail-interop.md) - the Rust hold this mirrors

## Related

- [JS session parity](/quest/m1/js-session-parity.md) - per-session caps, the other half
