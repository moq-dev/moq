# [XL] js/watch: playback runs in a worker, drawing to an OffscreenCanvas

## Goal

`@moq/watch` does its heavy lifting off the main thread: the connection,
subscription, container parsing, video and audio decoding, `Sync`, and
rendering into an `OffscreenCanvas`. A main-thread block shorter than the
playout delay is invisible, and one of any length only delays the UI. This is
the only path: where the required APIs are missing, playback refuses loudly
instead of falling back to the main thread.

## Plan

[Plan: watch worker](/quest/m1/plan-watch-worker.md) picks between an
invisible page-wide worker with on-demand signal handles and
application-spawned workers, and rewrites this quest with the result. Until
then, the settled constraints in that quest apply here too.

Adapt every consumer in the same PR: `<moq-watch>` and its UI, room, moq-boy,
and `demo/web`. Update `doc/lib/js/watch.md` (and `room.md` if its API moves)
inline with the threading model, the removed `frame` output, and any CSP
requirement.

Public API: removing `renderer.out.frame`, moving `Sync`, and changing what
`Player` and the composable classes accept are breaks to the published
`@moq/watch`, so the PR targets `dev`. The `@moq/signals` bridge is additive.
Wire: none.

Gate on the plan's jank harness and N-player sweep, both nightly.

## Required

- [Plan: watch worker](/quest/m1/plan-watch-worker.md) - picks the worker model and rewrites this quest
- [A/V clock](/quest/m0/plan-av-clock.md) - reshapes `Sync` and the worklet playhead, so the move to the worker happens once

## Related

- [#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - touches the same video decoder
- [Time stretch](/quest/m1/watch-audio-time-stretch.md) - changes the worklet this feeds
