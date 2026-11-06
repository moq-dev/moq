# [S] Stop publishing to JSR

## Goal

`@moq/*` packages publish only to npm. The JS release path no longer
generates `jsr.json` or runs `deno publish`, no workflow or Environment needs
a `JSR_TOKEN`, and the `@moq` packages on jsr.io are archived with a pointer
to npm, so nothing can publish to them again. Deno users import the npm
packages through `npm:` specifiers.

## Plan

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ drop JSR. Over the
preceding 30 days `@moq/net` had about 42 JSR downloads against about 341k
on npm, and `@moq/hang` about 38 against about 306k. `@moq/watch` and
`@moq/publish` never published there (they set `"jsr": false`), and most JSR
packages last published in June. Not chosen: keeping JSR behind a `JSR_TOKEN`
in the `npm` Environment, which was
[read-only pull-request CI](/quest/m0/ci/read-only-secrets.md)'s step for
JSR, whose repository OIDC trust cannot require an Environment.

- Remove JSR from `js/common/release.ts`, `js/common/package.ts` (the
  `jsr.json` generation and the `"jsr": false` opt-out), their tests, the
  `jsr` field in each `package.json`, and the `JSR_TOKEN` plumbing in
  `release-js.yml`. Packages publish from `release`, so backport the change
  there in the same PR, the way other release-workflow changes land.
- Rewrite doc comments and docs that link `jsr.io` (such as the
  `@moq/signals` re-export in `js/net/src/index.ts`) to npm.
- Archiving on jsr.io needs the maintainer's JSR login. Once the backport
  lands, they archive each `@moq` package with a description pointing to
  npm; check that `https://api.jsr.io/scopes/moq/packages` lists every
  package as archived. Archiving also ends the repository's OIDC link, so no
  separate unlinking is needed.

Public API: none in code; JSR stops receiving new versions. Wire: none.

## Related

- [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md) - its JSR token step is replaced by this quest
- [Claim the new @moq npm names](/quest/m3/release/npm-first-publish.md) - claims only npm names once JSR is gone
