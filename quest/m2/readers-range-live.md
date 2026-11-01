# [S] Latest readers subscribe with a relative range

## Goal

The readers that wait for the largest group and raise their floor to
start near it (moq-audio, moq-video, and both moq-room and `@moq/room` chat)
subscribe with a relative range instead, with no change in what they play:
`Range::live()` for a start at the newest group, and a relative start for a
reader that looks back, such as moq-video's warmup lane, which keeps its
one-group lookback so the preceding intra-refresh group still decodes.
`moq play` drops its floor entirely and starts where its consumers' `Delay`
puts them.

## Plan

Since [moq-dev/moq#6278](https://github.com/moq-dev/moq/pull/6278),
`Range::live()` starts at the live edge on a warm relay copy too.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01
/quest-afk run): `moq play` drops its one-group `lookback` range floor and
relies on the `Delay` start instead. Since
[moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311), its `open`
(`rs/moq-cli/src/play/media.rs`) passes each decoder's consumer a `Delay`:
the live decoder `{ min: video_delay, max: max_delay }` and the refresh
decoder `{ min + warmup, max + warmup }`, as `moq_video::decode::Consumer`
does. That start already decides where each decoder reads from, so the
`largest().await` plus `with_range(largest.group.saturating_sub(lookback)..)`
floor (and its copy in `play/video.rs`'s test helper `join`) is a second
start rule; #7311 kept it only to bound how long the refresh decoder's
start reach waits. Delete it rather than convert it to a relative range.
Check that the reason in its comment still holds without it (a warm relay
copy waiting on its source must not replay its backlog), and that the
refresh decoder still starts on the group below the live edge.

Inventory with `rg 'largest\(|poll_largest' rs/ js/` first, since Rust chat
polls rather than awaits; keep any caller that needs the position itself
rather than a start. Each reader keeps its existing tests,
plus one against a warm relay copy.

Public API: none expected. Wire: none.

## Required

- [JS ranges](/quest/m1/subscribe-ranges/js.md) - `@moq/room` chat needs relative ranges in `@moq/net`
