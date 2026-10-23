# [XS] Tooling tests gate every merge

## Goal

Every merge-queue Check runs `just gh test`, the tests of the CI selector,
release, size, and benchmark tooling, so a change to the machinery that picks
coverage cannot merge without its own tests. Nightly keeps running it as part
of the full gate.

## Plan

Decided 2026-10-22 from the repository improvement audit: run it on every
Check rather than select it by path. The whole recipe took about 10 s locally
(about 3% of a median 5.5-minute queue entry), and path selection would let a
broken selector skip the very tests that catch it.

- The run that planned this found `gh test` red on `main`:
  `sh/gh/setup.test.ts` expects Check to be the last step of its job, and
  [moq-dev/moq#6155](https://github.com/moq-dev/moq/pull/6155) appended
  "Upload build timings" after it. It stayed unnoticed because no PR runs the
  suite. [moq-dev/moq#6325](https://github.com/moq-dev/moq/pull/6325) fixes
  that fixture; land after it, or fix it here if it has not merged.
- Run it from the `gh` module's check in `sh/dispatch.sh` (or an always-selected
  module) and add any tools it needs (`python3`) to that module's tool list.
- Update the recipe comment in `.github/justfile` and the `gh test` comment in
  Nightly's gate matrix, which both say no pull request runs it. Keep it in
  Nightly only if the full gate there does not already cover it.
