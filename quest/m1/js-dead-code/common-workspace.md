# [S] js/common becomes a private workspace

## Goal

`js/common` is a private workspace package (`@moq/common`) that declares the
packages it imports, and every package that uses it declares a dependency on
it. A dependency tool then attributes each import to the package that makes
it. Public API: none, since the package is private. Wire: none.

## Plan

`js/common` holds the shared build scripts (`package.ts`, `release.ts`,
`api.ts`, `constructors.ts`, `deps.ts`) and the Vite plugins
(`vite-plugin-worklet.ts`, `vite-plugin-libav.ts`). Its imports of `vite`,
`rolldown`, and `typedoc` resolve today only because bun hoists them from
packages that list them. Meanwhile, consumers that reach the plugins through
`../../js/common/...` (`demo/web`, `demo/boy`, `js/publish`, ...) list
`rolldown` without importing it, so dependency tools report it as unused
there.

Decided 2026-11-17 (maintainer, /quest-plan): ✅ make it a workspace
(rejected: adding the imports to the root `devDependencies`; leaving it).

Recommended: consumers import the plugins as `@moq/common/...`, so the
dependency is visible in code, and drop `rolldown` and `vite` entries that
only existed for the plugin. Package scripts can keep `bun ../common/*.ts`
paths, since a script path is no import. About 70 references name
`js/common` or `../common`; change only the imports.

## Related

- [Dead export and manifest sweep](/quest/m1/js-dead-code/sweep.md) - the rest of the manifest fixes
