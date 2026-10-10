# [XS] Origin pool churn benchmark sweeps cursors

## Goal

`bench_pool_churn` in `rs/moq-net/benches/origin.rs` sweeps the number of
announce cursors on a prefix, alongside members and paths, so a renewal cost
that grows with cursors shows up as a slope.

## Plan

PR #5182 made a front's renewal check read each cursor presenting the prefix, and
a renewal restarts every cursor, so both scale with cursors. Add the axis, and
include a scoped cursor so the per-cursor scope filter is measured.

Public API: none. Wire: none.
