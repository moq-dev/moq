# [L] A nightly simulated event runs a relay cluster at audience scale

## Goal

`just bench event` (or the name the bench recipes settle on) runs one
publisher through a cluster of real `moq-relay`s (an origin, a middle tier,
and edges) to the viewer count
[the viewer-cost quest](/quest/m9/scale-sim/viewer-cost.md) measured,
all on virtual time over links that queue until they drop. It reports per relay
tier the instructions, memory, and bytes per delivered frame, and the viewers'
latency and stall percentiles, swept over viewer count and bufferbloat depth.
Nightly runs it on `main` and charts the series beside the other benchmarks,
within Kixel Quest's 8 MiB metrics budget.

## Plan

Recommended:

- Count instructions per simulated relay, since wall time on virtual time is
  meaningless and one process hosts every relay; attribute them per relay
  host, for example by running each relay's tasks on its own sim host and
  counting there.
- Viewers join in a burst and in a trickle, both: a join storm is when
  a live event costs most.
- Report the cipher's per-byte cost from the instruction benches beside the
  results, since the sim runs a null cipher.
- It never gates a merge; a regression past the series' noise files work the
  way the other nightly benchmarks do.

## Required

- [A simulated viewer costs as little as its real session does](/quest/m9/scale-sim/viewer-cost.md) - the viewer ceiling this runs at
