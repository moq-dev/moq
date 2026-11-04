# [XS] Bun runs only the test files we name

## Goal

Every scripted `bun test <file>` passes an exact path. Bun then never runs a
same-named copy elsewhere in the checkout, such as a generated release tree
under `target/` or a scratch directory. A check keeps new bare filters
from coming back.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7680](https://github.com/moq-dev/moq/pull/7680), first seen in
[moq-dev/moq#7669](https://github.com/moq-dev/moq/pull/7669)): milestone m0,
in the CI epic.

- Cause: bun treats a bare argument as a substring filter over discovered
  files. Only arguments that start with `./` or `/` are exact. The API
  checker's generated release tree under the build cache held copies of
  `sh/gh/*.test.ts`, and those old tests ran against main's workflows.
- Bare filters on main: `.github/justfile:30,54,59`
  (`sh/gh/alert|setup|ffi|cpp|publish.test.ts`),
  `test/json-properties.sh:12`, `test/resume-model.sh:12`,
  `test/justfile:52,54,277` (check what the harness lists expand to), and
  `test/smoke/justfile:122`. Prefix each with `./`.
- Guard: have the test inventory (`sh/gh/test-inventory.py`) or a small
  lint refuse a scripted `bun test` argument that ends in `.test.ts` and
  lacks `./` or `/`. If no root `bunfig.toml` exists, also consider one
  that limits discovery to the source roots.

Public API: none. Wire: none.
