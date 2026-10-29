# [XS] Log lock profile

## Goal

`just bench-profile lock video-heavy` runs before and after the epoch-based
`kio::log` ([moq-dev/moq#6782](https://github.com/moq-dev/moq/pull/6782)) on a
quiet profiling host (see `bench/README.md`), and shows no lock-contention
regression: `GroupState`'s share of mutex wait with multi-frame groups drops
or holds.

## Plan

- Compare the commit before #6782 with the one that merged it, at each worker
  count, and post the wait-by-stack diff on the PR that finishes this. The
  recipe's base revision
  ([moq-dev/moq#6680](https://github.com/moq-dev/moq/pull/6680)) covers the
  "before" side in one run:
  `just bench-profile lock video-heavy "1 4 16" tokio-workers d854bf98b1~1`.
- Decided 2026-10-27 (maintainer): the profiling host is the 24-core
  `desktop` workstation (`kernel.perf_event_paranoid=-1`), run while quiet:
  no other agents running and load near zero. That retired the profiling-host
  condition this waited on.
- #6782's body holds the criterion numbers, taken on an oversubscribed host:
  `group_contended` costs 13 to 61% less per reader across cores, and about
  4 ns more per frame on one thread. A profile that contradicts them goes back
  to the maintainer.
