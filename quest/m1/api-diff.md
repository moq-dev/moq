# [M] Committed JS API reports

## Goal

Each published `@moq/*` entrypoint's API Extractor report (`.api.md`) is
committed and checked on every PR, and the release gate's JS half diffs the
committed reports instead of the npm tarball's declarations. `release`
carries its own reports, so the first cut's diff has a baseline.

## Plan

Decided 2026-10-23 (maintainer interview, recorded on
[moq-dev/moq#6477](https://github.com/moq-dev/moq/pull/6477#issuecomment-6368834835)),
after that PR landed `just api-diff` and the release gate with the JS
baseline taken from the npm tarball:

- Commit the reports. CI regenerates them with `localBuild: false` and fails
  on a difference; `just fix` regenerates them.
- The release gate's JS half becomes a diff of the committed reports, and
  the npm-tarball path in `sh/api-diff/js.ts` is deleted. This replaces the
  earlier "JS baseline is the npm tarball" decision.
- Seed `release` once: a small backport commits `release`'s current reports
  and the per-PR report check.
- Keep one shared per-package API Extractor config, which
  [API Extractor checks](/quest/m1/api-extractor-checks.md) and the
  [reference](/quest/m1/js-api-reference.md) extend.
