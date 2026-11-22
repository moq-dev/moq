# [S] Speed up or split sh/rs/select.test.py

## Goal

`just gh test` finishes well under 25s instead of blocking ~50s on
`sh/rs/select.test.py`, without losing crate-selection regression coverage.

## Plan

After parallelizing `.github/justfile` `test` (173.48s to 51.79s),
`sh/rs/select.test.py` alone takes ~50s and is the sole critical path while all
other 45 tooling test scripts finish behind it.

- Profile the individual test cases in `sh/rs/select.test.py`.
- Reuse shared temporary git/cargo fixtures or run independent cases in parallel
  so fast regression coverage stays in `just gh test`.
- Move only genuinely expensive multi-crate integration fixtures to
  `sh/rs/select.slow.test.py` (covered nightly by `just gh test-integration` per
  `CONTRIBUTING.md`).

Decided 2026-11-21 (afk, `/quest-plan`):

- Placement: ✅ `quest/m0/ci/select-test-speed.md` (belongs in the active m0 CI
  cost and latency epic; rejected: top-level `m0`, `m1`, `m3`).
- Strategy: ✅ speed up or parallelize fixtures first, moving only inherently
  slow integration fixtures to `*.slow.test.py` (rejected: deleting tests;
  leaving `just gh test` blocked at ~50s).
