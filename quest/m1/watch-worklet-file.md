# [S] Watch audio under a strict CSP

## Goal

A page with CSP `script-src 'self'` plays watch audio. Today
`js/common/vite-plugin-worklet.ts` always exports the render worklet as a
`blob:` URL, so `addModule` is refused and audio never starts, with only a
console "spawn error".

## Plan

Decided: in `vite build`, emit the worklet as its own file and export
`new URL("<worklet>.js", import.meta.url)`; keep the blob URL only in
`vite serve`. Vite, webpack 5, and esbuild (with a plugin) resolve that
pattern inside a dependency. This applies to the publish capture worklet too
and deletes the runtime blob code. Document the consumer-bundler requirement.
The publish capture worker (`?worker&inline`) is out of scope.

Verify in a browser under `script-src 'self'` (Chromium and Firefox) and
from a Vite consumer app.

## Closes

- [#4323](https://github.com/moq-dev/moq/issues/4323) - close this issue when the quest finishes

## Related

- [JS bundle trims](/quest/m1/js-bundle-trims.md) - minifies worklets through the same plugin
- [Plan: watch worker](/quest/m1/plan-watch-worker.md) - notes the capture worker's `worker-src blob:` need
