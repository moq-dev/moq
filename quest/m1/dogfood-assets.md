# [S] Dogfood hosted worklets

## Goal

The moq.pro dashboard (`app/`) runs on the `@moq/*` release that ships
`assets()`. It hosts the worklet and worker files and calls
`Watch.assets()` / `Publish.assets()`, so the hosted path gets real traffic.
CSP is out of scope: the dashboard adds no policy.

## Plan

Decided in planning: the maintainer wants hosted mode dogfooded, not just the
pin bump. No CSP. moq.dev's half moved to
[its own quest tree](https://github.com/moq-dev/moq.dev/blob/main/quest/m0/dogfood-assets.md)
on 2026-10-09. The moq.pro side is part of its package adoption.

Guidance:

- Copy `node_modules/@moq/{watch,publish}/assets/*` into a served `/moq/`
  directory at build time. SvelteKit's Vite config takes
  `vite-plugin-static-copy` directly.
- Call `assets("/moq/")` once, before any element or player starts. The
  dashboard imports the element entries, so import `assets` from the package root.
- Verify in a browser that audio plays and publishes, and that the network
  panel shows the worklets and the capture worker (Firefox) loading from
  `/moq/`, not `blob:`.

## Required

- [A watch and publish release ships assets()](/quest/m1/assets-release.md) - the hosted files exist on npm to copy
