# [XS] The JS API reference regenerates only when its inputs change

## Goal

`doc/.vitepress/api/index.ts` skips TypeDoc when a hash of everything the
reference is built from matches the last run's, in the docs build and the dev
server alike, so a JS-module check that changes no input spends no TypeDoc
time.

## Plan

Measured 2026-11-12 on one of the maintainer's agent hosts: the reference
regenerated on every JS-module check (`doc`'s `check` script ends in
`vitepress build`, which never reuses it), 50 runs in 4.8 hours at a median
of 74 CPU-seconds, about 6 CPU-hours a day.

Decided 2026-11-12 (maintainer, /quest-plan): ✅ skip when a hash of the
inputs is unchanged.

- The hash replaces the `--cached` modification-time stamp, which every fresh
  checkout defeats; one rule serves build and dev server. Keep
  `MOQ_API_REFERENCE=regenerate` only if something still needs it.
- Inputs are what can change the output: the published packages' sources and
  `package.json`s, their tsconfigs and every config those reach through
  `extends`, such as `js/tsconfig.json`, the generator under
  `doc/.vitepress/api/`, and the TypeDoc versions and patches in `bun.lock`
  and `patches/`. A missed input serves a stale reference, so derive the
  list from what `generate` reads rather than by hand where possible.
- Recommended: keep the output keyed by its hash in a cache outside the
  checkout, written atomically, so each new agent worktree reuses it rather
  than regenerating once per worktree. Measure the hit rate across worktrees.

Public API: none. Wire: none.
