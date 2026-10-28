# [M] Archive time ranges

## Goal

A `Time(pts)` range on an `archive::Consumer` broadcast resolves from the
archive's key listing, not only from the groups the consumer's cache holds:
a start reaches the stored group containing pts, and an end stops just after
it, however old.

## Plan

Decided 2026-10-23 (maintainer, on
[moq-dev/moq#6262](https://github.com/moq-dev/moq/pull/6262)): this stays in
m2, required by [DVR rewind](/quest/m2/archive-dvr.md), rather than blocking
the archive consumer, which ships without time anchors.

The consumer creates each track live from its newest stored group and serves
older groups through `track::Fill`, so its cache holds almost nothing a time
anchor can resolve against: today a `Time(pts)` start older than the newest
group resolves to the oldest cached group instead. A `Fill` names group
positions only, so an anchor the cache can't resolve never reaches the
handler. [moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171) deferred carrying the anchor to the consumer quest, which
landed without it.

- `moq-net` passes a time anchor the cache can't resolve to the
  `track::Requests` handler, unresolved, in its fill.
- The consumer resolves it from its index: every object key carries its first
  frame's pts, so the object holding pts is one lookup, and the group within
  it comes from the stored group timestamps.

Test with the in-memory store: a start and an end older than the newest
group, an anchor before the oldest stored object, an anchor in a gap, and an
untimed track refusing it.

Public API: `track::Fill` gains the anchor. Wire: none.
