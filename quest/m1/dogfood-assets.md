# [S] Dogfood hosted worklets

## Goal

moq.dev (the Astro site plus the `sites/pub` and `sites/watch` Vite sites)
and the moq.pro dashboard (`app/`) run on the `@moq/*` release that ships
`assets()`. They host the worklet and worker files and call
`Watch.assets()` / `Publish.assets()`, so the hosted path gets real traffic.
CSP is out of scope: neither site adds a policy.

## Plan

Decided in planning: the maintainer wants hosted mode dogfooded, not just the
pin bump. No CSP on either site. The moq.dev change is a direct PR, since that
repo has no quest tree. The moq.pro side is part of its
[package adoption](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/ship-dev.md).
pronto/web is skipped: it renders video only and never loads a worklet.

Guidance:

- Copy `node_modules/@moq/{watch,publish}/assets/*` into a served `/moq/`
  directory at build time. moq.dev already lists `vite-plugin-static-copy` as
  an unused devDependency. Astro takes it through `vite.plugins`, and
  SvelteKit's Vite config takes it directly.
- Call `assets("/moq/")` once, before any element or player starts. The
  sites import the element entries, so import `assets` from the package root.
- Verify in a browser that audio plays and publishes, and that the network
  panel shows the worklets and the capture worker (Firefox) loading from
  `/moq/`, not `blob:`.

## Required

- [A watch and publish release ships assets()](/quest/m1/assets-release.md) - the hosted files exist on npm to copy
