# [M] Watch and publish under a strict CSP

## Goal

By default, watch and publish keep working on every bundler with no hosted
files: the worklets and the capture worker load from `blob:` URLs, as today.
An app whose CSP refuses `blob:` (`script-src 'self'`, `worker-src 'self'`)
opts in by copying the worker and worklet files out of `node_modules`,
hosting them, and giving each package their base URL. It then plays and
publishes audio with no CSP violations.

Covers the watch render worklet, the publish capture worklet, and the
publish capture worker (`?worker&inline`). Today a strict CSP refuses all
three, and the only symptom is a console "spawn error".

## Plan

Why not `new URL(..., import.meta.url)`: [#4485](https://github.com/moq-dev/moq/pull/4485)
emitted the worklets as assets behind that pattern. Built consumers probed
in Chromium on 2026-09-29 gave these results. Vite build, Vite dev
(prebundled), webpack 5, Rspack, Parcel, and import maps copied the file.
esbuild ESM, Rollup, and Bun left it behind (a 404, so no audio). esbuild
IIFE threw `Invalid URL` at module load, which broke the whole player. A
library can't require every consumer to host its files. And a strict CSP
can't avoid a same-origin file, because `addModule` and `new Worker` need a
URL, and only `blob:`/`data:` URLs come without hosting.

Decided in planning:

- One set of entry points, with the hosted mode set at runtime. No second
  entry tree and no export condition.
- The blob code sits behind a dynamic `import()` that only runs when no base
  is set. So the default loads it lazily, and hosted mode on a code-splitting
  bundler never downloads it. Single-file builds (esbuild without splitting,
  Bun, IIFE) inline it, which is about 12 KB minified of dead weight in hosted
  mode only. Accepted over doubling the exports.
- Hosted-mode files load from `new URL(<fixed name>, base)`, with the base
  first resolved against `document.baseURI`, so a root-relative `/moq/`
  works (`new URL(name, "/moq/")` alone throws). Never `import.meta.url`:
  the default path must not reference an asset that some bundlers drop.
- API: one global base URL per package, set once before playback (e.g.
  `Watch.assets("/moq/")` and `Publish.assets("/moq/")`; the exact name is for
  review). Assets are app-wide, so nothing threads through `Player`, the
  decoder, or the elements. room and moq-boy set nothing of their own; their
  docs say to set both.
- Files: stable, unhashed names under each package's `dist/assets/` (e.g.
  `@moq/watch/assets/render-worklet.js`), also listed in `package.json`
  `exports` so copy plugins and `import.meta.resolve` can find them. The
  worklet messages can change between versions, so the docs say to recopy on
  every upgrade.
- `vite serve` in this repo keeps blob URLs.
- Docs: an inline section in `doc/lib/js/watch.md` and
  `doc/lib/js/publish.md` (not a shared page). Each covers the base URL, the
  files to copy, and the CSP directives. The capture worker needs
  `worker-src`, the worklets `script-src`.
- Tests:
  - A `js/common` unit test in `just js test`, with no browser. It asserts
    the built default dist has no `new URL(..., import.meta.url)` asset
    reference and that the blob code is only reached through `import()`.
  - A manual bundler-matrix script, not wired into CI or nightly. This is the
    maintainer's call, an exception to the repo's CI rule. It builds a
    consumer with Vite, webpack, esbuild IIFE, and Bun, then loads each in
    Chromium, in default mode (no CSP) and hosted mode (strict CSP). The page
    loads all three assets directly (both worklets through `addModule`, and
    the capture worker through a spawn and a message round trip). Chromium's
    publish path uses the main-thread `MediaStreamTrackProcessor` and never
    spawns the worker, and the worker path silently falls back when the
    spawn fails. Report the results in the PR.

Reuse from #4485 where it fits: the `resolveFileUrl`/`emitFile` plumbing in
`js/common/vite-plugin-worklet.ts` and its Playwright CSP page, minus the
interop CI step.

## Closes

- [#4323](https://github.com/moq-dev/moq/issues/4323) - close this issue when the quest finishes

## Related

- [JS bundle trims](/quest/m1/js-bundle-trims.md) - minifies worklets through the same plugin
- [Plan: watch worker](/quest/m1/plan-watch-worker.md) - a new watch worker joins the same assets base
