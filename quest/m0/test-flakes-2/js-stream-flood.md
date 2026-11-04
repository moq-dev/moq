# [S] @moq/net's uni-stream flood test survives reruns

## Goal

`js/net/src/stream.test.ts`'s "a flood of uni streams yields to the host's
other tasks" passes under `bun test --rerun-each` and under load. If it fails,
the cause is fixed: either the test's own state leaking between reruns, or
the accept loop failing to yield.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7658](https://github.com/moq-dev/moq/pull/7658)): milestone m0,
a child of the test-flakes epic.

- #7658's exploratory stress run (`--rerun-each 5`) failed this test once.
  It passes in normal suites and CI, and that run left no log.
- The test is at `js/net/src/stream.test.ts:1001`, unchanged since
  [moq-dev/moq#7386](https://github.com/moq-dev/moq/pull/7386). It stubs
  `performance.now` to add 5 per call to a module-level `fakeNow`. It then
  expects a `yieldNow()` sibling to run before 1024 accepts finish. Suspect
  first that `fakeNow` and the stub carry over between reruns, or are
  restored late.
- Reproduce first: `bun test --rerun-each 50` on the file, both alone and
  next to the rest of `js/net`. Then use the epic's load harness. Fix the
  cause, whether in the test or in `stream.ts`'s budget, and keep the
  test's claim that the flood yields.

Public API: none. Wire: none.
