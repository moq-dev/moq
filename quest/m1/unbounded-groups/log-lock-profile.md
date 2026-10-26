# [XS] Log lock profile

## Goal

`just bench-profile lock video-heavy` runs before and after the epoch-based
`kio::log` ([moq-dev/moq#6782](https://github.com/moq-dev/moq/pull/6782)) on a
host with `kernel.perf_event_paranoid=-1`, and shows no lock-contention
regression: `GroupState`'s share of mutex wait with multi-frame groups drops
or holds.

## Plan

- Compare the commit before #6782 with the one that merged it, at each worker
  count, and post the wait-by-stack diff on the PR that finishes this.
- #6782's body holds the criterion numbers, taken on an oversubscribed host:
  `group_contended` costs 13 to 61% less per reader across cores, and about
  4 ns more per frame on one thread. A profile that contradicts them goes back
  to the maintainer.

## Required

- [A profiling host](/quest/m1/perf-host.md) - the `perf_event_paranoid=-1` host the lock profile runs on
