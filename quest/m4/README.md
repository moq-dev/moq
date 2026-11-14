# m4: later work, lower priority

## Goal

Work that follows the next breaking release with nobody waiting on it yet:
tests, tooling, benchmarks, optimizations, conditions on outside parties that
only later work needs, and features only a downstream deployment's later
plans need.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan): this milestone requires the
[release cut](/quest/m3/release/cut.md), so until it lands spare capacity goes
to m0 to m3 and then to [m9](/quest/m9/README.md)'s lab work (rejected:
requiring m0, m1, and m2, which lifts only when they empty, and m0 rarely
does; also gating m3, which holds the cut). The cut's PR deletes the cut
quest, which lifts this gate for good: from then on m4 holds each cycle's
wire cut, which it can't require.

Decided 2026-11-05 (maintainer, milestone re-cut): the lower half of the old
m2 ("later work"), as [m3](/quest/m3/README.md) describes. After each cut
m4's quests move to m3 and m4 starts empty. A quest here that gains a
consumer moves to m3; one that loses its last reason moves to m5 or is
deleted.

## Required

- [Release cut](/quest/m3/release/cut.md) - the cut lands before this milestone's work starts
