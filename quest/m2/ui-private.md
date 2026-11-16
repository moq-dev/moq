# [M] @moq/ui is a workspace package, bundled into watch and publish

## Goal

`@moq/ui` is `private: true` and never reaches npm. `@moq/watch` and
`@moq/publish` bundle its code and its types, so their published packages
import nothing from it and no cross-package contract hides behind
`@internal`. `@moq/video` exports nothing marked `@internal` to another
package either. The npm name is not claimed, and the docs, the API reports,
and the release list drop it.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Workspace-private and bundled (rejected: keeping it published with the
  helpers as a typed public API under `just api-diff`; keeping it as is with
  watch and publish pinned to its exact version). The package exists to
  share code between two packages, which is a build concern, and its name
  promises a player UI it does not hold.

Facts found by the audit, re-checked at `80ca9787e3`:

- Its public API is one abstract class, `Support`, and four types
  (`js/ui/src/index.ts:18`). What watch and publish take at runtime is
  `controlButton`, `fullscreenButton`, `settingsButton`, `icon`, `graph`,
  `statCard`, `statLine`, and four formatters, all tagged `@internal`
  (`js/ui/src/index.ts:10-17`), from 17 import sites (10 in `js/watch`, 7
  in `js/publish`).
- `stripInternal` removes those from the published types, but the published
  watch and publish keep every `@moq/*` import external
  (`js/watch/vite.config.ts:25`, `js/publish/vite.config.ts:23`) and depend
  on `^` of the workspace version (`js/common/package.ts:85`). So changing
  an `@internal` helper in a patch breaks an installed watch or publish:
  the contract is real, untyped, and outside the API reports.
- `Support`, `Report`, and `Show` are part of watch's and publish's own
  public types (`js/publish/api/support-element.api.md:4-7`,
  `js/watch/src/support/element.ts:9-12`), so bundling must inline the
  types, not only the code.
- `@moq/video` does the same thing once: `rotateVideoDimensions` is
  `@internal` (`js/video/src/index.ts:26-27`) and imported by
  `js/watch/src/video/decoder.ts:18`, `js/publish/src/broadcast.ts:7`, and
  `js/publish/src/preview.ts:99`.
- Nothing is published under the name: `@moq/ui` returned 404 on
  2026-11-12 ([first publish](/quest/m3/release/npm-first-publish.md)).
  `js/crypto` is already `private: true`.
- The package is 783 lines of source and CSS, so a page loading both
  overlays carries that much twice.

For the PR:

- Set `private: true`, drop the `release` script, and remove `@moq/ui`
  from each vite `external` rule and from the dependencies `package.ts`
  rewrites, so the build fails if a published package still imports it.
  `just js check` already rejects an undeclared `@moq/*` import; make it
  reject a published package depending on a private one.
- Bundle the types: the support elements' declarations must resolve
  without `@moq/ui` installed. The `test-dts` checks in `js/publish` are
  where to prove it, with one for `js/watch`.
- `stats.css` (`js/ui/package.json` exports `./stats.css`) is bundled the
  same way.
- For `@moq/video`, recommended: export `rotateVideoDimensions` as public
  API, since it is a five-line pure helper (alternative: inline it in the
  two callers).
- Drop the name from `doc/lib/js/index.md`, `doc/setup/upgrade.md`,
  `doc/.vitepress/api/package.json`, and `js/ui/api/`. Delete
  `js/ui/MIGRATE.md`; the README says what the package is for inside the
  workspace.
- Measure `@moq/watch/element` and `@moq/publish/element` first-load size
  before and after with `sh/size/bundles.ts` and put both in the PR.

Public API: `@moq/ui` is never published; `@moq/watch` and `@moq/publish`
keep the same exported names, now declared in their own types. `@moq/video`
gains one export. Wire: none.

## Related

- [Claim the new @moq npm names](/quest/m3/release/npm-first-publish.md) - no longer claims `@moq/ui`
