# [S] moq-room scales to large rooms

## Goal

A benchmark measures `moq-room`'s `Room` with large participant counts
(for example 10, 100 and 1,000 followed paths), covering the cost of one
participant's change and of steady-state wakes. If the cost grows with room
size per wake, the room polls only the followers a wake concerns, and the
benchmark shows the change.

## Plan

Planned 2026-10-31 (`/quest-plan`, maintainer), a follow-up reported by the
[Listing and follow](/quest/m1/api-audit/route/events.md) implementation:
`Room` now holds a follower per listed path and polls every follower on each
wake, which is fine at room sizes seen today. Benchmark first, as
[Keyed prefix polling](/quest/m2/keyed-prefix-polling.md) does, and fix
only if it shows a slope. Wire the benchmark into the existing bench lanes
([Benchmark sources](/quest/m2/bench-sources.md)).

Public API: none. Wire: none.

## Required

- [Listing and follow](/quest/m1/api-audit/route/events.md) - the follower-per-path room this measures
