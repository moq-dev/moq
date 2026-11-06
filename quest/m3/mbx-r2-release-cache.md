# [XS] Release pull requests read the R2 Rust cache

## Goal

A release pull request's `Check the cache` step reports `effective
read-only`, and the PR that closes this records it. Condition: `release`
carries the platform Rust cache, which its `platform.yml` still lacks on
2026-11-05; check with
`git show origin/release:.github/workflows/platform.yml | grep rust-cache`.

## Plan

Decided 2026-11-05 (maintainer, /quest-audit): ✅ split from
[Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) and moved to m2 (m3 since the milestone re-cut)
behind [the cut](/quest/m3/release/cut.md), which brings `main`'s platform
Rust cache to `release` when it merges `main` into it. Not chosen: moving
the whole R2 verification quest; merging `main` into `release` now, which
reverses the 2026-11-04 decision.

Once the cut lands, inspect the first release pull request's `Check the
cache` step.

Public API: none. Wire: none.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - brings the platform Rust cache to `release`
