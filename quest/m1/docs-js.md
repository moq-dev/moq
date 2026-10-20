# [M] Document and enforce the JS public exports

## Goal

Every export reachable from a published JS package's `exports`, and every
public member of an exported class, has a TSDoc comment, and `just check`
fails when a new one lacks it.

## Plan

The m0 documentation audit (2026-10-19) documented the gaps in `@moq/auth`,
`@moq/hang`, `@moq/json`, and `@moq/net`. What remains is in the UI packages:
`@moq/publish` (37 exports, 54 class members), `@moq/watch` (28 exports, 64
members), and `@moq/boy` (2 exports, 30 members). About 40 of those have a
`//` comment that only needs to become `/** */`.

Nothing enforces doc comments for JS: Biome has no such rule, and there is no
TypeDoc or TSDoc config. Recommended: a TypeScript compiler API check in
`js/common` beside `deps.ts` and `declarations.ts`, run from
`sh/js/workspace.py check`. It walks each package's `exports` through its
re-export chains, so it sees the public surface rather than every `export`
in a file, and needs no new dependency. Exempt symbol-keyed brand members
such as `@moq/signals`' `[SIGNAL_BRAND]`. `deno doc --lint` also covers this
but cannot disable its slow-types rule, and `eslint-plugin-jsdoc` would add a
second linter that only sees per-file exports.
