# [M] Test cost audit

## Goal

The slowest, flakiest, and least valuable tests are sped up, fixed, deleted,
or planned, so the suite costs what it earns. Workflows and jobs belong to the
[CI workflow audit](/quest/m1/audit-ci.md).

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
and `retries = 0` is deliberate. A test that sleeps on the wall clock should
mock time (AGENTS.md). Delete tests whose coverage another test already
provides. Failed Nightly, Interop, and Platform jobs, and what each job costs, moved to
the [CI workflow audit](/quest/m1/audit-ci.md) (2026-10-20).

What the m0 run (2026-10-19) fixed: the ACME rate-limit test's released port,
the stats session graph's recursive Send proof (send-depth and fuzz),
bench/ffi-gen's stale wasm-bindgen lock, moq-uring's fixed 1 ms calibration
slack, the Nightly JS benchmark stall, the CI selection test's feature count,
and the stats producer benchmark lint. It planned
[redundant failover drift](/quest/m1/test-flakes-2/redundant-failover-drift.md)
and [macOS QUIC connects](/quest/m1/test-flakes-2/macos-quic-connect.md).

Baselines from queue run
[39371990135](https://github.com/moq-dev/moq/actions/runs/39371990135):
Test took 17 min and Check 15 min; nextest ran 7,657 tests in 220 s. The
slowest were the three new redundant-importer exports (37, 29 and 15 s,
waiting out a killed importer's 10 s QUIC idle timeout), the CLI
restart/stitch case (18.6 s), same-instance linger (17.5 s), TS source-limit
timing (16.5 s), the impaired flapping-peer drill (16.4 s), and the one-day
jitter drift simulation (14.7 s). Apart from the redundant exports, the
leaders match the earlier sample and cover different behaviors.

Still to measure: a JS net benchmark-only edit selects WASM and the net
dependents, and a stats benchmark-only edit selects relay and CLI tests, docs,
and CLI's optional play build. Compare that cost with the regression signal
before changing selection.
