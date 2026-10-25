# [M] API Extractor checks for @moq/* declarations

## Goal

Every published `@moq/*` entrypoint is checked by API Extractor on every PR,
with its messages as errors: a type the public API uses but does not export
(`ae-forgotten-export`), a declaration that does not resolve, and an exported
item without a doc comment (`ae-undocumented`) fail the build.
`js/common/docs.ts` and `js/common/declarations.ts` are deleted.

## Plan

Decided 2026-10-23 (maintainer interview):

- Builds on the per-package API Extractor config and committed `.api.md`
  reports that [the API-diff check](/quest/m1/api-diff.md) adds
  ([moq-dev/moq#6477](https://github.com/moq-dev/moq/pull/6477)). One config
  serves the reports, these checks, and the
  [reference](/quest/m1/js-api-reference.md); do not add a second.
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

## Required

- [API-diff check](/quest/m1/api-diff.md) - adds the shared config and committed reports these checks extend

## Related

- [Generated JS API reference](/quest/m1/js-api-reference.md) - renders the doc model from the same config
