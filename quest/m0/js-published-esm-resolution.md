# [S] js: the published @moq/hang and @moq/msf cannot be imported by Node

## Goal

Every entry point `@moq/hang` and `@moq/msf` document resolves from their
published tarballs under Node, not just under a bundler or inside the bun
workspace.

## Plan

`tsc` emits relative import specifiers exactly as the source writes them, and
these two packages write directory and extensionless ones. The published
`dist/index.js` of `@moq/hang` carries `import * as Catalog_1 from "./catalog"`,
which Node's ESM resolver refuses: it does no directory-index lookup and no
extension guessing. `@moq/net` is already correct, writing `"./announced.js"`,
so the fix is to bring the other packages to the convention `@moq/net` uses
rather than to change the build.

Nothing in the repository sees this today. `just check` type-checks sources,
`vite` and every bundler resolve the specifiers happily, and
`test/smoke/clients/js-native` imports `@moq/hang/catalog` under Node but
resolves it through the bun workspace to TypeScript source rather than to the
published `dist`. Only a consumer installing the tarball hits it, which is why
`just test packaged` found it and nothing else did.

- Give every relative import in `js/hang` and `js/msf` an explicit `.js`
  specifier, with a directory import becoming `<dir>/index.js`. Check the other
  publishable packages for the same shape rather than assuming these two are
  alone.
- Prefer a lint that keeps it fixed (Biome's `useImportExtensions`, or the
  TypeScript `rewriteRelativeImportExtensions`/`moduleResolution: nodenext`
  setting the packages would need) over a one-time sweep, so the next
  extensionless import fails in `just check`.
- Remove both names from `NODE_IMPORT_BROKEN` in `test/packaged/js.sh`, which
  fails the run once they resolve, and confirm `just test packaged --js` imports
  every `@moq/hang` and `@moq/msf` entry point under Node.
