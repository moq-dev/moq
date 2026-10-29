# [S] No constructible internal handles

## Goal

No exported JS class can be built from its emitted declarations with
arguments that skip its factory. `new Rendition()`, `new Lister()`, and
`new Arrivals()` fail to type-check instead of failing at runtime (`Lister`
throws inside its constructor, `Rendition` on `close()`). A lint rule
rejects any new `@internal` constructor.

## Plan

Decided 2026-10-27 from the release-to-main API audit (maintainer picked each
option):

- The bug: `js/tsconfig.json` sets `stripInternal`, so an `@internal`
  constructor disappears from the declaration. The class then shows an
  implicit public zero-argument constructor. The audit reproduced
  `new Rendition()` type-checking and then throwing `this.#close is not a
  function`. Both are already on `release`.
- **Every instance**, published and private:
  `Rendition` ([js/publish/src/rendition.ts](/js/publish/src/rendition.ts),
  built by `Broadcast`), `Lister`
  ([js/net/src/listing.ts](/js/net/src/listing.ts)), `Arrivals`
  ([js/watch/src/arrivals.ts](/js/watch/src/arrivals.ts)), and the two
  constructors in [js/e2ee/src/generation.ts](/js/e2ee/src/generation.ts)
  (the package is private, but the same mistake stays fixed).
- **The fix is the `Origin` pattern**
  ([js/net/src/origin.ts](/js/net/src/origin.ts)): a real `private
  constructor` plus a module-level factory assigned in the class's `static {}`
  block. A factory used from another module, such as `Rendition`'s from
  `broadcast.ts`, must not escape through the package's `export *`. Put it in
  a module the entry point does not re-export. Factories stay the only entry
  point; documentation alone or another `@internal` tag does not prevent the
  construction.
- **Guard**: a Biome GritQL plugin (Biome 2.5 is in use) registered in
  `biome.jsonc` flags a `constructor` carrying an `@internal` JSDoc tag, so
  `just check` fails on a recurrence. If Biome cannot see the JSDoc tag,
  write a declaration test that asserts each exported class's constructor
  arity.
- Verify against the emitted declarations, not the sources: a
  `@ts-expect-error` fixture compiled against each package's built types
  shows `new X()` no longer type-checks.

Public API: breaking only for code that called these constructors, which
always produced broken handles. Wire: none.
