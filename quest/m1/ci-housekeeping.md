# [XS] Finish CI housekeeping

## Goal

- The merge queue's nextest runs with `NEXTEST_TEST_THREADS=24` after loaded
  validation passes.
- `.github/actionlint.yaml` drops its ignore for the `queue` key once an
  actionlint release after 1.7.12 is in the dev shell.

## Plan

- **Threads.** Recommended in
  [moq-dev/moq#6174](https://github.com/moq-dev/moq/pull/6174): the suite
  spends most slots waiting. Run a few loaded `just check --all` passes at
  24 before setting it for the queue job only. A full Rust test selection
  with 24 workers pinned to eight CPUs failed
  `export::redundant_importers_switch_to_the_survivor_and_back_on_lite_07`
  ("stitching export exited") and
  `stats::a_late_sink_reads_the_final_stats`
  ("the import exited before its input ended"). nextest stopped after
  1,870 of 9,398 tests: 1,868 passed, two failed.

  The stats failure reproduced on main `005eca04aa` with inherited
  `RUST_LOG=warn`: its INFO readiness message was suppressed, and the test
  reported failure when the importer finished its normal 10 s retention.
  The child now explicitly sets `RUST_LOG=info`, like the other CLI fixtures
  that wait for log messages. All four stats integration tests pass with
  inherited `RUST_LOG=off`, including the late sink in 140 ms; production
  behavior and timeouts are unchanged.

  [moq-dev/moq#6113](https://github.com/moq-dev/moq/pull/6113) still owns the
  redundant-importer fix. Account for that prerequisite, then complete the
  loaded validation before enabling 24 threads. No retry, assertion, timeout,
  or concurrency setting changes in this checkpoint.
- **Actionlint.** [rhysd/actionlint#661](https://github.com/rhysd/actionlint/pull/661)
  is merged but unreleased. The dev shell still has actionlint 1.7.12.
  [moq-dev/moq#6180](https://github.com/moq-dev/moq/pull/6180) added the
  ignore for the three Pages publishing workflows; remove it when the
  release reaches nixpkgs and `just gh check` accepts `queue: max`.

Public API: none. Wire: none.

## Related

- [CI workflow audit](/quest/m1/audit-ci.md) - the broader per-job review
