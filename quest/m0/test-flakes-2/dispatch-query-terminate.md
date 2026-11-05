# [XS] dispatch-query's terminate test survives load

## Goal

`sh/dispatch-query.test.py`'s `test_terminating_the_run_stops_the_running_phase`
passes reliably under host load, fixed at its cause under the epic's rules.

## Plan

Planned 2026-11-04 (maintainer) from the stream-credit scheduler
([moq-dev/moq#7894](https://github.com/moq-dev/moq/pull/7894)), whose
`just check` saw it fail once on a loaded host while it passed 3/3 alone;
that PR does not touch `sh/`. Reproduce it under load (repeated runs beside
a CPU-heavy job), find the deadline or ordering it races on, and assert on
the event instead of a wait, as the epic's rules say.
