# [S] Merges go through a merge queue

## Goal

A pull request cannot break `main` by merging on checks that ran against an
older `main`. [#4137](https://github.com/moq-dev/moq/pull/4137) did exactly
that: it merged cleanly, but combined with
[#4089](https://github.com/moq-dev/moq/pull/4089) it stopped `moq-ffi` from
compiling on `main` until [#4157](https://github.com/moq-dev/moq/pull/4157)
fixed it. The `main` ruleset requires `Check` and `Test` but not strictly,
and there is no merge queue, so nothing re-runs them on the combined tree.

## Plan

- Decided: a GitHub merge queue, not a strict ruleset. Strict status checks
  would force every open PR to update and re-run whenever `main` moves; a
  queue tests the combination once, at merge time.
- Make the workflows ready: every workflow providing a required check
  (today `Check` and `Test` in `.github/workflows/check.yml`) also runs on
  `merge_group`. `just ci $JOB $BASE` (`justfile`) scopes by diffing
  against its `BASE` argument, else `origin/$GITHUB_BASE_REF`. A merge group
  sets no `GITHUB_BASE_REF`, so pass the group's base
  (`github.event.merge_group.base_sha`) as `BASE`. Check the concurrency
  group and the `closed`-only skip still behave for queue refs.
- Document it in `CONTRIBUTING.md`: PRs merge through the queue, a
  dequeued PR means the combination failed, and how agents enqueue (the
  merge skills use `gh pr merge`, which enqueues when a queue is on).
- The ruleset change (enable the queue, choose squash) is the maintainer's
  act, after the workflow change lands on `main`. Hand it over with the
  settings to use rather than changing it.

Public API: none. Wire: none.
