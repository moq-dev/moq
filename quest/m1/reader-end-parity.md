# [S] Readers see a track's end at the same moment in Rust and JS

## Goal

A track reader in `@moq/net` sees the end as soon as the newest group reaches
the declared end, as Rust readers already do, instead of waiting for the track
to close after the tail settles. The same subscription ends for a reader at
the same point in both languages.

## Plan

Found in [#4533](https://github.com/moq-dev/moq/pull/4533): the tails agree
on when a subscription settles, but the reader-facing end differs. The Rust
track model ends for readers once the newest group reaches the declared end,
so a hole waiting out the grace never delays a reader. JS readers wait for the
track to close, grace included.

Decided with the maintainer: adopt Rust's behavior in JS. A reader isn't held
for the grace over groups it would skip anyway; the tail still settles
separately for accounting. Keep a clean end that has not settled distinct from
an abort, as #4120 and #4116 require.

Tests: the same scenario in both languages (a declared end with a missing
group in the grace window) reports the end to readers without waiting out the
grace. Consider asserting it across languages in [Track tail
interop](/quest/m1/track-tail-interop.md).

Public API: none expected; a JS behavior change. Wire: none.

## Related

- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - names every missing group, so fewer holes reach the grace at all
