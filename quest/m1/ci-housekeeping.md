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
  1,870 of 9,398 tests: 1,868 passed, two failed. Investigate these failures
  before enabling the setting; don't mask them with retries or longer
  timeouts. [moq-dev/moq#6113](https://github.com/moq-dev/moq/pull/6113)
  already edits the redundant-importer tests.
- **Actionlint.** [rhysd/actionlint#661](https://github.com/rhysd/actionlint/pull/661)
  is merged but unreleased. The dev shell still has actionlint 1.7.12.
  [moq-dev/moq#6180](https://github.com/moq-dev/moq/pull/6180) added the
  ignore for the three Pages publishing workflows; remove it when the
  release reaches nixpkgs and `just gh check` accepts `queue: max`.

Public API: none. Wire: none.

## Related

- [CI workflow audit](/quest/m1/audit-ci.md) - the broader per-job review
