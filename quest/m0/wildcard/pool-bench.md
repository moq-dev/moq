# [S] Pool resolution benchmark

## Goal

A benchmark sweeps pool size against requested-path count for route
resolution, so a cost that grows with the pool or the path set instead of the
touched path shows up as a slope.

## Plan

[#4279](https://github.com/moq-dev/moq/pull/4279) keyed `route_order`'s tie
break on a hash of the requested path, so resolving a path now scans the
equal-cost pool and hashes the path with every candidate's hops
(`rs/moq-net/src/model/origin.rs`). Only a correctness test
(`equal_cost_pool_spreads_paths`, 4 members by 64 paths) covers it. Codex
asked for the two-axis sweep AGENTS.md requires for fan-out
([r4117485535](https://github.com/moq-dev/moq/pull/4279#discussion_r4117485535)),
and the PR listed it as a known gap. The maintainer ruled in the 09-28
merged-PR audit that it blocks the line.

Add it beside the existing origin benchmarks in `rs/moq-net/benches/`. If the
slope shows resolution scaling with the pool in a way that matters, say so in
the PR rather than optimizing here. The PR's other known gap, tracks per
front for the driver's per-event admission walk, is worth sweeping in the
same change if it is cheap.

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - the line this blocks
