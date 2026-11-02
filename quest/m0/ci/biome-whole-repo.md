# [S] Biome checks every file it can parse

## Goal

`just check` runs Biome over the whole repository, not only `js`, `bench/browser`, `test`, `doc`, and `demo`, and main is clean under it: the 16 problems Biome reports today in `.github/`, `cpp/`, `rs/`, and `sh/` are fixed.

## Plan

Found by the agent on [moq-dev/moq#7305](https://github.com/moq-dev/moq/pull/7305). Biome's paths are listed in `sh/js/workspace.py:201` and `js/justfile:45`, so these files are never checked:

- JSON formatting: `.github/ci-selection-audit.json`, `cpp/obs/buildspec.json`, `rs/hang/fixtures/*.json`, `rs/moq-pattern/tests/pattern.json`.
- TS formatting: `sh/gh/cpp.test.ts`, `sh/size/bundles.ts`.
- `noTemplateCurlyInString` in `sh/gh/setup.test.ts:109-119`, whose plain strings hold GitHub's `${{ ... }}`; suppress it there with a reason rather than rewrite the fixtures.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Fix the files and widen the paths in one PR so the gate stays green. Exclude generated or vendored trees (fixtures written by tools, `node_modules`, build output) through `biome.json` rather than by listing directories in two scripts. The bigint `useMathMinMax` false positive in Biome 2.6 belongs to #7305; no upstream report was approved.

Public API: none. Wire: none.
