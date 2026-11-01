# [S] Latest readers subscribe with a relative range

## Goal

The readers that wait for the largest group and raise their floor to
start near it (moq-audio, moq-video, `moq play`, and both moq-room and
`@moq/room` chat) subscribe with a relative range instead, with no change in
what they play: `Range::live()` for a start at the newest group, and a
relative start for a reader that looks back, such as moq-video's warmup lane,
which keeps its one-group lookback so the preceding intra-refresh group still
decodes.

## Plan

Since [moq-dev/moq#6278](https://github.com/moq-dev/moq/pull/6278),
`Range::live()` starts at the live edge on a warm relay copy too.

Inventory with `rg 'largest\(|poll_largest' rs/ js/` first, since Rust chat
polls rather than awaits; keep any caller that needs the position itself
rather than a start. Each reader keeps its existing tests,
plus one against a warm relay copy.

Public API: none expected. Wire: none.

## Required

- [JS ranges](/quest/m1/subscribe-ranges/js.md) - `@moq/room` chat needs relative ranges in `@moq/net`
