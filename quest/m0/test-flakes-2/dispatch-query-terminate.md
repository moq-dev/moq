# [XS] dispatch-query's terminate test survives load

## Goal

`sh/dispatch-query.test.py`'s `test_terminating_the_run_stops_the_running_phase`
passes reliably under host load, fixed at its cause under the epic's rules,
and its `@unittest.skip` is removed.

## Plan

Planned 2026-11-04 (maintainer) from the stream-credit scheduler
([moq-dev/moq#7894](https://github.com/moq-dev/moq/pull/7894)), whose
`just check` saw it fail once on a loaded host while it passed 3/3 alone;
that PR does not touch `sh/`. It is skipped meanwhile, per AGENTS.md's rule
to disable a flaky test at once. Reproduce it under load (repeated runs beside
a CPU-heavy job), find the deadline or ordering it races on, and assert on
the event instead of a wait, as the epic's rules say.

A lead to verify: pullfrog reproduced a failure 1 in 420 runs on bash 5.3.20,
where the EXIT trap's `wait` returns a cancelled lane's 143 and `set -e` skips
`report`. Its fix, `wait || true` in `sh/dispatch.sh`'s EXIT trap
([e6690144ca](https://github.com/moq-dev/moq/commit/e6690144ca14f9ec8c48a56b4ed26c5a65a47708)),
passed 420/420 but was dropped from
[moq-dev/moq#7789](https://github.com/moq-dev/moq/pull/7789) as out of scope.
