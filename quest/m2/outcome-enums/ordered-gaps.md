# [S] An ordered track's next_group yields groups

## Goal

`track::Ordered::next_group` (and `poll_next_group`) yields group consumers
and skips the gaps itself, so no caller writes a `Gap => continue` arm. A new
`next_item` (and `poll_next_item`) yields `track::Item` for the one reader
that needs gaps in order: moq-mux's container reader.

## Plan

Decided 2026-11-18 (maintainer, /quest-plan): ✅ two methods (rejected:
keeping `Item` as the only return; replacing gaps with a counter, which
[unused-api/net](/quest/m2/unused-api/net.md)'s reader cannot use, since it
needs each gap's range in order).

Every production caller skips `Item::Gap` today: libmoq, moq-ffi, moq-json
snapshot and window, moq-hls, moq-mux's TS export and MSF catalog,
moq-archive, moq-stats, and moq-net's benches
(`rg 'Item::Gap|Item::group'`). Tests and benches that write
`.and_then(Item::group)` read a gap as the end of the track; they move to
`next_group`. `Item::group` goes if nothing needs it after.

Whichever lands second of this and [unused-api/net](/quest/m2/unused-api/net.md)
points the container reader at `next_item`.

Public API: breaking in moq-net; callers of `next_group` that matched `Item`
drop the match, and the gap-reading one calls `next_item`. No JS or binding
change: js/net's ordered reads and moq-ffi already hide gaps. Wire: none.

## Related

- [moq-net's last pub items](/quest/m2/unused-api/net.md) - moves the container reader onto `Ordered`'s gaps
