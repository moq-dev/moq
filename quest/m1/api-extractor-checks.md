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

Implementation checkpoint: strict compiler, extractor, and TSDoc messages are
wired into the shared report configuration. The JSR `@module` tag is defined
and translated to `@packageDocumentation` in the isolated declaration copy;
focused fixtures cover missing exports, stripped names, and undocumented
exports. The old checkers remain until the replacement can pass the workspace.

Open decision: the isolated copy intentionally cannot resolve external
packages, so strict compiler diagnostics reject every bare dependency import
with TS2307. Choose whether to preserve isolation with narrowly scoped external
module diagnostics, or resolve declared dependency types without relying on
sibling packages having been built. The former keeps today's independent
reports and is the smaller change, but makes the unresolved-declaration check
explicitly local to the package. No diagnostic exception is implemented yet.

Remaining after that decision: migrate existing release tags and TSDoc syntax,
replace private-helper `typeof` references in public static method bags with
explicit signatures, fix any other exposed public declarations without
expanding exports unnecessarily, remove `docs.ts` and `declarations.ts` with
their superseded tests, and pass `just check` and `just ci`.

## Related

- [Generated JS API reference](/quest/m1/js-api-reference.md) - renders the doc model from the same config
