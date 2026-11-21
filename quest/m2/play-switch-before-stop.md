# [M] moq play opens a replacement rendition before stopping the playing one

## Goal

When `moq play` switches video rendition while one is playing, because a
catalog snapshot moves or retires it or the window turned past square, the
picture keeps playing until the replacement has resolved its broadcast,
subscribed, and opened its decoder. A replacement that never comes (an
offline sibling broadcast, or a subscription left unanswered) leaves the old
rendition playing. A later turn back or catalog change cancels the pending
replacement instead of waiting it out.

## Plan

Today the player stops the old rendition first (`Demand::retire_moved`, and
the viewport turn in `play_broadcast`). Then it picks inline:
`source.resolve(...)` and `track.subscribe(...)` are awaited outside the
`select!` that watches the catalog and the viewport. A turn now switches only
to a broadcast already in hand (`reachable`), but a subscription can still
block. A rendition on a sibling that is still being discovered isn't taken
until the next turn.

Open the replacement in a future raced in that `select!`, keep the old task
until it is ready, then swap. `Playback`'s half has to tell the old task's
end apart from the new one's, for example with a per-task generation, so the
old task ending doesn't re-arm selection. A discovery that resolves while the
window is still turned should then switch on its own.

## Related

- [Players pick a rendition by orientation](https://github.com/moq-dev/moq/pull/9287) - added the viewport turn
