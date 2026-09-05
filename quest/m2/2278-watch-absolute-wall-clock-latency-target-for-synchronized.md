# [S] hang: a timeline consumer exposes the wall anchor

## Goal

A browser application can read a rendition's `Timeline.wall` and its records
through a `Timeline.Consumer` in `js/hang`, so an application that knows its
viewers share a clock can compute the delay that renders one frame at one
instant everywhere, and a DVR view can map presentation time to wall time.

The library itself never synchronizes playback on wall time. That is the
decision behind [#2278](https://github.com/moq-dev/moq/issues/2278): frame
timestamps are relative by design, `Timestamp::now()` is a one-way bridge with
a per-process jitter to deter wall-clock readings, and two machines' clocks are
only comparable when the application already knows they are synced. No
absolute `delay` mode, no client clock estimation from the session RTT, and no
sync exchange over a track.

## Plan

`js/hang` has a timeline producer (`setWall(pts, wall)`) and no consumer;
`js/watch/src/sync.ts` anchors on first-frame arrival against
`performance.now()`. Add the consumer beside the producer, mirroring the Rust
timeline consumer's records and exposing `wall` as a signal, and keep `Sync`
untouched. Document in `doc/concept` how an application derives a `delay`
from `wall + pts` against its own clock, and what it gives up when the clocks
are not synced.

Nothing populates `wall` from the built-in publishers today, so this waits on
[Publishers anchor the timeline](/quest/m2/timeline-wall.md).

## Required

- [Publishers anchor the timeline](/quest/m2/timeline-wall.md) - there is no anchor to expose until publishers set one

## Closes

- [#2278](https://github.com/moq-dev/moq/issues/2278) - close this issue when the quest finishes
