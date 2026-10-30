# [S] Worklet bundler matrix and React hooks run in CI

## Goal

`just js worklet-matrix` runs nightly against pinned bundler versions, and
`@moq/signals/react`'s `useValue` and `useSignal` have tests.

## Plan

From the 2026-10-27 coverage audit.

- `js/common/worklet-matrix.ts` checks that `?worklet` output survives Vite,
  webpack, esbuild, and Bun consumers, including under a strict CSP, but says
  "Not in CI". It also runs `bun add webpack webpack-cli` unpinned at run
  time. Pin the bundlers through the repo's lockfile so a night's failure
  means a change on our side or a deliberate bump, then add it to Nightly.
- `js/signals/src/react.ts` exports two hooks that no test exercises. Cover
  mount and unmount, switching the signal a component reads, functional
  updates through `useSignal`'s setter, and that unmount drops the
  subscription.

Public API: none. Wire: none.
