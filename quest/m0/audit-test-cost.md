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

Use nextest's normal CI log to rank the slowest tests (`slow-timeout` is 30s).
Flakes are tracked by hand in [test flakes](/quest/m1/test-flakes-2/README.md),
and `retries = 0` is deliberate. Mine nightly and merge-queue failures since
the previous run with `gh run list` for new ones. A test that sleeps on the
wall clock should mock time (AGENTS.md). Delete tests whose coverage another
test already provides. Rank CI jobs by duration. [CI cost and
latency](/quest/m0/ci/README.md) owns runner and cache infrastructure.

Audit in progress (2026-10-19): Nightly runs
[39266265648](https://github.com/moq-dev/moq/actions/runs/39266265648) and
[39124936449](https://github.com/moq-dev/moq/actions/runs/39124936449) both
spent their full hour in JS benchmarks. The forward fixture never completed
its fake peer's initial set, making every watcher repeatedly compare every
pending scope. It also let the real two-second discovery linger expire while
measuring setup. Syncing the empty peer and holding logical time fixed makes
the full 8/128/2048 watches by 1/8/32 sessions sweep finish with its existing
cost checks. The original production implementation passes that repaired
benchmark; no production optimization is needed.

The same nights exposed a stale exhaustive-feature count in the CI selection
test after WebRTC was added. Its Bash fixture also assumed `/bin/bash`,
which breaks on NixOS. The stats producer benchmark needs the same documented
real-clock lint exemption as its neighboring decode and aggregate benchmarks.
The unused stats benchmark import is separately fixed by the dead-code audit.

Queue run [39216656008](https://github.com/moq-dev/moq/actions/runs/39216656008)
records 7,227 passing test durations before failing. The longest passes were
the CLI restart/stitch case (19.14s), same-instance linger (17.77s), impaired
flapping-peer drill (16.89s), TS source-limit timing (15.13s), and one-day
jitter drift simulation (14.93s). These cover different behaviors; no test
retirement is justified by this sample.

Remaining investigation: that queue run's
`acme::tests::failed_validations_stop_short_of_the_rate_limit` failed its
30-second watchdog at `rs/moq-relay/src/acme/tests.rs:576`. These tests use
real sockets and rustls-acme's wall-clock polling. The cause is not established;
do not extend the watchdog or substitute paused Tokio time around real I/O.
The selected branch check passed this case in 6.16s without reproducing it.
Keep this run open until that failure is reproduced and fixed or scoped into
an independently reviewable investigation. Do not advance the recurrence yet.

Other costly Nightly jobs in the October 18-19 sample included macOS
platform coverage (56-57 minutes), binding benchmarks (43-57 minutes), Rust
bench targets (40-51 minutes), and audio quality (47-60 minutes). Their work
differs and belongs outside the PR gate; runner/cache changes stay with the
CI epic. Queue run
[39287133351](https://github.com/moq-dev/moq/actions/runs/39287133351) failed
the Python clock test with `Old`:
[moq-dev/moq#5944](https://github.com/moq-dev/moq/pull/5944) made an expired group fail its pending
first-frame read. [moq-dev/moq#5962](https://github.com/moq-dev/moq/pull/5962)
restored the skip to the later group, so the test keeps its original assertion.

The branch check also shows a scoping cost worth measuring next: a JS net
benchmark-only edit selects WASM and the net dependents, while a stats
benchmark-only edit selects relay/CLI tests, docs, and CLI's optional play
build. Compare that cost with the regression signal before changing selection.

Verified locally: `just check`, the full forward benchmark sweep, the 8 CI
selection tests, and targeted stats producer-benchmark Clippy. The stats
benchmark check temporarily used the dead-code audit's import cleanup; that
prerequisite is excluded from this PR.
