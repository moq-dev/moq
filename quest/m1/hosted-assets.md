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

Found by the 2026-11-07 API shape audit (browser media). Move the resolver
into `@moq/hang`, the shared dependency whose `Util.Libav.polyfill()`
(`js/hang/src/util/libav.ts`) loads libav, as a small per-package directory
helper, and have the polyfill take its base from the package that loads it.
`@moq/watch`'s and `@moq/publish`'s `assets()` keep their signatures and
their own directories, so an app that hosts each package's files in a
different directory keeps working and nothing breaks; each also sets the
libav base on the `@moq/hang` instance it imports, so a page that resolves
two `@moq/hang` copies still configures the one each package uses. Each
package's `assets/` ships the libav wasm it needs. `@moq/libav` stays an
implementation dependency. Future hosted files (the watch worker) join the
same call. Test it on the CSP demo pages, including watch and publish
configured with different directories.
