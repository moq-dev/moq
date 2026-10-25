# [S] ts_schedule regression

## Goal

`rust/shared/ts_schedule/{1/1, 1/16, 4/1}` (`rs/moq-mux/benches/ts_schedule.rs`)
return to their cost before the regression, and Nightly's paired gate on
the shared bench targets passes.

## Plan

Nightly's first paired run (0513c07369 against 4381af6f0) measured them
400 to 460% slower, too large for noise. The window holds
[moq-dev/moq#6417](https://github.com/moq-dev/moq/pull/6417),
[moq-dev/moq#6533](https://github.com/moq-dev/moq/pull/6533), and
[moq-dev/moq#6527](https://github.com/moq-dev/moq/pull/6527). Bisect, then
fix the cost at its source, or show the bench now measures more work and
rebaseline with that reason. The smaller `server_admission` (+20%) and
`uring-session_lite/no-gso` (+11%) deltas belong to the threshold
calibration in [bench CI](/quest/m1/bench-ci.md).

Public API: none. Wire: none.
