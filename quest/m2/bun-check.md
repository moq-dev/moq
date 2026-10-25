# [S] Typecheck with bun check

## Goal

Every workspace package's `check` script, and the bench typechecks in
`js/justfile` and `test/json-properties.sh`, run `bun check` instead of
`tsc --noEmit`. `tsc -b` builds keep TypeScript for declaration emit.

## Plan

Waits on nixpkgs shipping bun 1.4.4 or later (nixpkgs-unstable carried 1.4.2
on 2026-10-22). `bun check` arrived in 1.4.3, but 1.4.4 adds the
`--no-script` the `check` scripts below need and fixes 1.4.3's false errors.
Bump `flake.lock` then; no bun overlay before it (decided 2026-10-22).

Measured 2026-10-22, bun 1.4.4 against TypeScript 7.0.2 on `main`: identical
errors and exit codes on all 33 `tsc` invocations in the `check` scripts and
bench typechecks, including 18 planted errors across 11 codes. The 33 targets
dropped from 6.7s to 1.9s in total; `js/watch` went from 307 ms to 77 ms with
2.3x less memory.

Repeat that diff on the bumped bun and switch only on an exact match: run each
target under both with `--pretty false` / `--no-pretty` and compare the sorted
error lines. A `check` script becomes `bun check --no-script`, since a bare
`bun check` runs the package's `check` script instead. `tsc -b
examples/tsconfig.json` becomes `bun check -b -p examples/tsconfig.json`.
