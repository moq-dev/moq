# [S] Seed release with API reports

## Goal

`release` carries its own committed API reports and the per-PR report check,
so the first cut's API diff compares against a baseline instead of nothing.

## Plan

- Decided by the maintainer on [moq-dev/moq#6477](https://github.com/moq-dev/moq/pull/6477):
  seed `release` once with a small backport. Today main has 36 `.api.md`
  reports and release has none, and `sh/api-diff/run.sh` reads them from
  `origin/release`.
- Backport `js/common/api.ts` and the report check, and commit the reports
  generated from release's own sources.
