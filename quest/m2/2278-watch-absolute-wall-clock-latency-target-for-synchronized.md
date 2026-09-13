# [S] hang: a timeline consumer exposes the wall anchor

## Goal

A browser application can read the broadcast's fixed PTS-to-wall mapping
through `js/hang`, alongside archive timeline records when present, so an application that knows its
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

Expose the catalog contract selected by the continuous broadcast clock quest,
using one mapping across tracks and source restarts. The current timeline is
a broadcast-wide segment index, not a per-rendition track. Reuse the existing
consumer and signal machinery where present; do not assume the old `setWall`
producer or create per-record clock epochs. Keep `js/watch` arrival-based Sync
unchanged. Document PTS-to-wall conversion and the requirement that an
application knows whether remote clocks are synchronized.

Nothing populates `wall` from the built-in publishers today, so this waits on
[Publishers anchor the timeline](/quest/m1/broadcast-clock.md).

## Required

- [Publishers anchor the timeline](/quest/m1/broadcast-clock.md) - there is no anchor to expose until publishers set one

## Closes

- [#2278](https://github.com/moq-dev/moq/issues/2278) - close this issue when the quest finishes
