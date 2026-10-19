# [M] Test and CI cost audit

## Goal

The slowest, flakiest, and least valuable tests and CI jobs are sped up, fixed,
deleted, or planned, so the suite costs what it earns.

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.

Starting points (2026-10-19): nextest reports no durations. Use its JUnit or
libtest-json output to rank the slowest tests (`slow-timeout` is 30s).
Flakes are tracked by hand in [test flakes](/quest/m1/test-flakes-2/README.md),
and `retries = 0` is deliberate. Mine nightly and merge-queue failures since
the previous run with `gh run list` for new ones. A test that sleeps on the
wall clock should mock time (AGENTS.md). Delete tests whose coverage another
test already provides. Rank CI jobs by duration. [CI cost and
latency](/quest/m0/ci/README.md) owns runner and cache infrastructure.
