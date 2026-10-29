# [M] Paired bench regressions bisected

## Goal

Each regression the paired Rust comparison confirmed on 2026-10-27 is fixed at
its cause or shown to be noise by an A/A rerun, and Nightly's Rust bench
targets job passes.

## Plan

Nightly [40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673) (2026-10-27, at 22808bbb2f) confirmed five regressions:
retirement/selection at 8x32, 32x32, and 128x32 (+21 to 24%),
server_admission/workers=4 (+10%), and track_range_demand/1000r (+10%).
The retirement/selection rows agree across three sizes, so treat them as
real: bisect with the paired base across the previous night's commits. The two
10% rows sit at the threshold that [bench CI](/quest/m1/bench-ci.md) has yet
to calibrate; rerun them before bisecting. bench-ci owns the threshold, this
quest owns the regressions.

Public API: none. Wire: none.

## Related

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - the threshold these crossed
