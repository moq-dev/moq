# [XS] The test inventory sees JS bench scripts

## Goal

The test-inventory gate resolves the JS bench scripts Nightly runs, so a JS bench entry point no workflow runs is flagged like any other opt-in target.

## Plan

Found by the agent on [moq-dev/moq#7316](https://github.com/moq-dev/moq/pull/7316): Nightly runs every JS bench through the `javascript` helper as `"$@"` (`sh/gh/nightly.sh:66-72`, the loop at `:138-155`), which the checker can't resolve, and `test-inventory.py` skips scripts under `/bench/`. Every JS bench entry point has a caller today, so this closes a coverage gap in the checker, not in CI.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Make the bench list something the checker can read (a static list Nightly loops over, or a recipe per bench) rather than teaching the checker shell indirection, and stop skipping `/bench/`.

Public API: none. Wire: none.
