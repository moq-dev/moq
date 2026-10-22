# [S] Latest readers subscribe with Range::live()

## Goal

Condition: [moq-dev/moq#6278](https://github.com/moq-dev/moq/pull/6278) has
merged, so `Range::live()` starts at the live edge on a warm relay copy too.

Then the readers that wait for `largest()` and raise their floor to start at
the newest group (moq-audio, moq-video, `moq play`, moq-room chat) subscribe
with `Range::live()` instead, with no change in what they play.

## Plan

Inventory with `rg 'largest\(\)' rs/` first; keep any caller that needs the
position itself rather than a start. Each reader keeps its existing tests,
plus one against a warm relay copy.

Public API: none expected. Wire: none.

## Related

- [Audio join budget](/quest/m2/audio-join-budget.md) - moq-audio's join at the latest group
- [Track live signal](/quest/m1/track-live.md) - the fix this builds on
