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
and `retries = 0` is deliberate. A test that sleeps on the wall clock should
mock time (AGENTS.md). Delete tests whose coverage another test already
provides. [CI cost and latency](/quest/m0/ci/README.md) owns runner and cache
infrastructure. Once the one-time [CI pipeline audit](/quest/m0/ci/audit.md)
closes, this audit ranks job costs again.

What the m0 run (2026-10-19) learned; check these first:

- Classify every failed job of the latest Nightly, scheduled Interop, and
  Platform runs, not just the slowest. The m0 run's first checkpoint looked
  mainly at the Nightly's stalled JS benchmarks, while 28 Nightly jobs had
  failed for eight independent causes, some for days. A red Nightly hides the
  next regression: the send-depth gate and all six fuzz targets broke on
  2026-10-18 and stayed broken unnoticed until this audit.
- Platform (macOS, Windows) runs only in the merge queue and is not required,
  so a Windows-only compile error in moq-video stayed red for two days across
  dozens of queue runs until
  [#5966](https://github.com/moq-dev/moq/pull/5966). Most Platform failures
  in the sample were that error, a choco `nasm` install outage, or another
  queued PR's breakage. Rank Platform by failure cause, not count.
- A queue failure often belongs to another PR in the same batch: a stale
  fixture in the entry itself, or a semantic conflict with a PR merged after it
  was queued. Check the entry's head branch and what merged around it before
  calling it a flake.
- Fixed by the m0 run: the ACME rate-limit test's released port, the stats
  session graph's recursive Send proof (send-depth and fuzz), bench/ffi-gen's
  stale wasm-bindgen lock, moq-uring's fixed 1 ms calibration slack, the
  Nightly JS benchmark stall, the CI selection test's feature count, and the
  stats producer benchmark lint.
- Planned by the m0 run: [redundant failover drift](/quest/m1/test-flakes-2/redundant-failover-drift.md),
  [macOS QUIC connects](/quest/m1/test-flakes-2/macos-quic-connect.md), and
  [nightly media players](/quest/m1/media-engines-nightly.md). Owned
  elsewhere: release builds by [release builds link](/quest/m0/release-link.md),
  and Rust advisories by the dependency audit. The Nix wasm-bindgen CLI now
  follows `Cargo.lock` ([#6031](https://github.com/moq-dev/moq/pull/6031)).

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
