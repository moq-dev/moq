# [S] JS release refuses a package whose @moq dependency is missing

## Goal

`js/common/release.ts` refuses to publish a package to npm when any
`@moq/*` dependency range in its built `package.json` resolves to no
published version, and names the missing ones. A failed dependency publish
can no longer leave a dependent on npm that fails to install.

## Plan

Decided 2026-10-22 (maintainer, from the release audit): requested as a
follow-up by [moq-dev/moq#6243](https://github.com/moq-dev/moq/pull/6243).

`release-js.yml` runs `bun --filter '*' release`, which orders packages by
dependency but still runs dependents after a dependency's script fails.
`release.ts` skips a version already on npm and otherwise publishes; its only
`@moq/*` existence check gates the JSR dry run. `js/common/package.ts`
rewrites `workspace:^` to `^<version>` without checking npm.

Before `npm publish`, read the dependencies and peer dependencies of
`dist/package.json`, run `npm view <name>@<range> version` for each `@moq/*`
entry, and exit non-zero listing every range with no match. Reuse the
existing JSR dry-run lookup instead of adding a second one. Confirm that
`deno publish` already refuses an unresolvable `jsr:` import; extend the
check to JSR only if it does not.

Prove it on a dry run that depends on an unpublished `@moq/*` version: the
run fails, naming the package and range.

Public API: none. Wire: none.

## Related

- [First npm publish](/quest/m2/release/npm-first-publish.md) - the one-time hand publish of the new names this check would otherwise refuse
