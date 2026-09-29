# [XS] hang: document the broadcast wall clock

## Goal

A browser application can find how to map presentation time to wall time
from `doc/lib/js/hang.md`, so an application that knows its viewers share a
clock can compute the delay that renders one frame at one instant everywhere,
and a DVR view can label its timeline.

The library itself never synchronizes playback on wall time. That is the
decision behind [#2278](https://github.com/moq-dev/moq/issues/2278): frame
timestamps are relative by design, `Timestamp::now()` is a one-way bridge with
a per-process jitter to deter wall-clock readings, and two machines' clocks are
only comparable when the application already knows they are synced. No
absolute `delay` mode, no client clock estimation from the session RTT, and no
sync exchange over a track.

## Plan

The API already exists: the catalog root's optional `clock` (`ClockSchema`,
`Clock`) and `wallClockTime(clock, pts, ptsTimescale)` in
`js/hang/src/catalog/clock.ts`, exported from `@moq/hang/catalog`. Only the
docs are missing. Add a short section to `doc/lib/js/hang.md`: read `clock`
from a catalog root, convert a frame's PTS with `wallClockTime`, note that a
live-only broadcast carries it without an `archive` entry, that the mapping is
fixed for the broadcast's life, and that comparing wall times across machines
requires the application to know their clocks are synchronized. Keep
`js/watch` arrival-based Sync unchanged.

## Closes

- [#2278](https://github.com/moq-dev/moq/issues/2278) - close this issue when the quest finishes
