# [S] One assets() for every hosted file

## Goal

A page that serves the player's files itself (for a strict
`Content-Security-Policy` with no `blob:` workers) calls one `assets(url)`,
and that call also points the libav AAC polyfill at the hosted wasm. Today
`@moq/watch` and `@moq/publish` each carry an identical copy of `assets()`
(`js/watch/src/assets.ts`, `js/publish/src/assets.ts`), and an app that
needs the polyfill must also import `@moq/libav` and set its mutable
`LibAV.base`, a transitive dependency our API never mentions.

## Plan

Found by the 2026-11-07 API shape audit (browser media). Move `assets` and
its resolver into `@moq/hang`, the shared dependency whose
`Util.Libav.polyfill()` (`js/hang/src/util/libav.ts`) loads libav, and have
the polyfill take its base from it. `@moq/watch` and `@moq/publish`
re-export it, so their existing `assets()` callers keep working and nothing
breaks. `@moq/libav` stays an implementation dependency. Future hosted files
(the watch worker) join the same call. Test it on the CSP demo pages.
