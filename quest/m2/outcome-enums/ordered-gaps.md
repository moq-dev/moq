# [S] An ordered track's next_group yields groups

## Goal

`track::Ordered::next_group` (and `poll_next_group`) yields group consumers
and skips the gaps itself, so no caller writes a `Gap => continue` arm. A new
`next_item` (and `poll_next_item`) yields `track::Item` for the one reader
that needs gaps in order: moq-mux's container reader.

## Plan

Decided 2026-11-18 (maintainer, /quest-plan): ✅ two methods (rejected:
keeping `Item` as the only return; replacing gaps with a counter, which
moq-mux's container reader cannot use, since it needs each gap's range in
order and when what it gave up landed).

Every production caller skips `Item::Gap` today: libmoq, moq-ffi, moq-json
snapshot and window, moq-hls, moq-mux's TS export and MSF catalog,
moq-archive, moq-stats, and moq-net's benches
(`rg 'Item::Gap|Item::group'`). Tests and benches that write
`.and_then(Item::group)` read a gap as the end of the track; they move to
`next_group`. `Item::group` goes if nothing needs it after.

moq-mux's container reader (`container/consumer.rs`, which reads through
`Ordered` since [moq-dev/moq#9196](https://github.com/moq-dev/moq/pull/9196))
moves onto `next_item`, the one production caller that reads gaps.

Public API: breaking in moq-net; callers of `next_group` that matched `Item`
drop the match, and the gap-reading one calls `next_item`. No JS or binding
change: js/net's `Ordered` already has `nextGroup` and `nextItem`, and moq-ffi
hides gaps. Wire: none.
