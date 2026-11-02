# [S] Fanout takes the route lock less

## Goal

A fanout round takes moq-net's route lock far fewer than the 41 times per round
[moq-dev/moq#7493](https://github.com/moq-dev/moq/pull/7493) measured, and the
lock's share of mutex wait falls.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Profile with `just bench-profile lock fanout`, and remove each acquisition
that reads state already held. Also make `bench/profile.py lock` accept
`tokio-rt-worker` threads, which #7493 flagged. `GroupExpiry::is_expired` is
covered by [track-state reads](/quest/m2/perf/cursor-version-skip.md).
