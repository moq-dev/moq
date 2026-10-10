# [S] JS readers hold for a track's pending tail

## Goal

A `@moq/net` reader of a received track waits for missing groups below a
declared end until the tail settles, as Rust readers do since #4225,
instead of ending at the end with groups missing (`doc/lib/js/net.md`).

## Plan

Mirror the Rust pending-tail hold from #4225 (`rs/moq-net/src/model/track.rs`)
and test it against the Rust behavior with mocked time. Split from [JS session parity](/quest/m1/js-session-parity.md) on
2026-10-08 so the caps work stays ready.

Mirror the arrival record from [Tail arrivals](/quest/m1/tail-arrivals.md)
rather than #4225's cache scan (decided 2026-10-10), so JS ports the final
design once.

Public API: none. Wire: none.

## Required

- [Tail arrivals](/quest/m1/tail-arrivals.md) - Rust judges tail holes from recorded arrivals and errors a short tail

## Related

- [JS session parity](/quest/m1/js-session-parity.md) - per-session caps, the other half
