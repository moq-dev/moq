# [S] The JS worklet library-build test survives load

## Goal

"library builds load worklets from a lazy blob or a hosted base, leaving no
child process" (`js/common/worklet.test.ts`) passes reliably in a loaded
merge-queue `just ci`, fixed at its cause, never by raising a timeout or
adding a retry. Done when the test no longer fails the queue and a loaded
local repeat of it passes.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`). It timed out in the
merge-queue batch for [moq-dev/moq#7536](https://github.com/moq-dev/moq/pull/7536)
([run 41700523666](https://github.com/moq-dev/moq/actions/runs/41700523666),
2026-11-03 19:13Z):

```text
#[group]js/common/worklet.test.ts:
(fail) library builds load worklets from a lazy blob or a hosted base, leaving no child process [5004.51ms]
  ^ this test timed out after 5000ms.
(pass) the dev server serves hosted worklets where builds emit them [803.45ms]
```

This test already failed the queue once and was counted fixed in the epic's
2026-10-26 entry. It runs a real `vite build` of a one-line worklet, so a
loaded host can push one build past bun's default 5-second test timeout.

- Measure where the time goes under load: Vite's startup, the plugin's
  in-process worklet bundle, or a wait on a child process.
- If the build is slow because the plugin does needless work, make it cheaper.
  If it is a whole Vite build that no smaller unit can replace, move the check
  to a cheaper level, for example by testing the plugin's transform without
  a full build, and keep one build where it is cheap enough to stay reliable.
- A longer per-test timeout is not a fix under the epic's rules.

Follow the rules in [More tests under load](/quest/m0/test-flakes-2/README.md).
