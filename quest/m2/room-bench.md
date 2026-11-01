# [S] moq-room scales to large rooms

## Goal

A benchmark measures `moq-room`'s `Room` with large participant counts
(for example 10, 100 and 1,000 followed paths), covering the cost of one
participant's change and of steady-state wakes. If the cost grows with room
size per wake, the room polls only the followers a wake concerns, and the
benchmark shows the change.

## Plan

Planned 2026-10-31 (`/quest-plan`, maintainer), a follow-up to the
required [Listing and follow](/quest/m1/api-audit/route/events.md)
implementation, after which `Room` will hold a follower per listed path and
poll every follower on each wake. That should be fine at room sizes seen
today. Benchmark first, as
[moq-dev/moq#7340](https://github.com/moq-dev/moq/pull/7340) did for the
lite subscriber's prefixes, and fix
only if it shows a slope. Wire the benchmark into the existing bench lanes
([Benchmark sources](/quest/m2/bench-sources.md)).

Public API: none. Wire: none.

## Required

- [Listing and follow](/quest/m1/api-audit/route/events.md) - the follower-per-path room this measures
