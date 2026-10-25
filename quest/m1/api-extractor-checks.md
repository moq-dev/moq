# [M] API Extractor checks for @moq/* declarations

## Goal

Every published `@moq/*` entrypoint is checked by API Extractor on every PR,
with its messages as errors: a type the public API uses but does not export
(`ae-forgotten-export`), a declaration that does not resolve, and an exported
item without a doc comment (`ae-undocumented`) fail the build.
`js/common/docs.ts` and `js/common/declarations.ts` are deleted.

## Plan

Decided 2026-10-23 (maintainer interview):

- Builds on the shared API Extractor config in `js/common/api.ts` and the
  committed `.api.md` reports that
  [moq-dev/moq#6477](https://github.com/moq-dev/moq/pull/6477) added. One
  config serves the reports, these checks, and the
  [reference](/quest/m1/js-api-reference.md); do not add a second. It
  analyzes a copy of `dist/` with no `node_modules`, so a package's report
  never depends on its dependencies being built, and silences every message
  for now.
- Keep only the rules API Extractor enforces. A `docs.ts` rule with no
  API Extractor equivalent is dropped, not kept in a reduced checker.
- JSR reads `@module` blocks, so define `@module` in `tsdoc.json` instead of
  switching entrypoints to `@packageDocumentation`. API Extractor reads an
  entrypoint's summary only from a block tagged `@packageDocumentation`, so
  add that tag beside `@module` (or an equivalent transform) to keep module
  summaries in the doc model for the reference.
- `stripInternal` stays: rolling declarations up with release tags
  (`dtsRollup`) is a non-goal.
- Prove the replacement: reintroducing the `@moq/auth` `AccessInput` bug
  and dropping an export's doc comment must each fail `just check`.
- Rust is a non-goal: docs.rs and cargo-semver-checks cover it.

Decided 2026-10-25 (maintainer): resolve declared dependency types without
requiring sibling packages to be built, accepting a broader redesign of the
analysis and its inputs, rather than scoping external-module diagnostics.
Reason: an unresolved declaration should fail wherever it is, not only inside
the package, and no blanket suppression of external-module diagnostics.

- The `dist/` copy gets a `node_modules` holding only the declared
  dependencies, peer and optional dependencies, linked to their workspace
  install. A workspace sibling resolves to its source through its `exports`,
  so it never needs to be built; an undeclared import fails with TS2307.
- A dependency's own declarations are checked by its own build: messages
  located in files outside the copy are ignored, by location, not by code.
- One compiler state covers every entrypoint. A forgotten export is accepted
  when every same-named declaration in this package the reporting signature
  references is nameable from some entrypoint: exported, a namespace member,
  or renamed by a nameable declaration (`type A = B`, `x: typeof f`).
- `export * as X` is rewritten to import-then-export, since API Extractor
  treats each namespace re-export as a new namespace.
- `ae-missing-release-tag` is off (`@internal` is the only tier) and
  `ae-unresolved-link` is off (JSR resolves links in lexical scope).

## Related

- [Generated JS API reference](/quest/m1/js-api-reference.md) - renders the doc model from the same config
