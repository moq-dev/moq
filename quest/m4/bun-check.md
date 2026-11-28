# [XS] Typecheck remaining JS benches in check scripts

## Goal

Every package with a `bench/` directory typechecks it in its `check` script
using `bun check --no-script`, so a removed export breaks the merge gate instead
of Nightly.

## Plan

The `tsc --noEmit` to `bun check --no-script` migration across all workspace
`check` scripts, `js/justfile`, and `test/json-properties.sh` is complete on
Bun 1.4.4 (`tsc -b` builds keep TypeScript for declaration emit).

What remains (from `main` at c681b7a09, added 2026-11-12 by `/quest-plan`):

- Add `bench/tsconfig.json` and include `bun check --no-script -p bench/tsconfig.json`
  in the `check` script for `js/clock`, `js/qmux`, `js/signals`, and `js/watch`.
- Move the `js/json` and `js/publish` bench typechecks into their package
  `check` scripts alongside the existing checks in `js/net`, `js/e2ee`,
  `js/hang`, and `js/pattern`.
