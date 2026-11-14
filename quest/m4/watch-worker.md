# [XL] js/watch: playback runs in a worker, drawing to an OffscreenCanvas

## Goal

`@moq/watch` does its heavy lifting off the main thread: the connection,
subscription, container parsing, video and audio decoding, `Sync`, and
rendering into an `OffscreenCanvas`. A main-thread block shorter than the
playout delay is invisible, and one of any length only delays the UI. This is
the only path: where the required APIs are missing, playback refuses loudly
instead of falling back to the main thread.

## Plan

[Plan: watch worker](/quest/m4/plan-watch-worker.md) picks between an
invisible page-wide worker with on-demand signal handles and
application-spawned workers, and rewrites this quest with the result. Until
then, the settled constraints in that quest apply here too.

Adapt every consumer in the same PR: `<moq-watch>` and its UI, room, moq-boy,
and `demo/web`. Update `doc/lib/js/watch.md` (and `room.md` if its API moves)
inline with the threading model, the removed `frame` output, and any CSP
requirement.

Public API: removing `renderer.out.frame`, moving `Sync`, and changing what
`Player` and the composable classes accept are breaks to the published
`@moq/watch`. The `@moq/signals` bridge is additive.
Wire: none.

Gate on the plan's jank harness and N-player sweep, both nightly.

Changed 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): [moq-dev/moq#8296](https://github.com/moq-dev/moq/pull/8296) ships `Video.RendererOutput` without `frame` and narrows `@moq/room` before the cut, so those two breaks are gone from this quest.

## Required

- [Plan: watch worker](/quest/m4/plan-watch-worker.md) - picks the worker model and rewrites this quest
