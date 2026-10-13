# [S] JS changes test only affected packages

## Goal

A PR touching JS, docs, drafts, or the test directory runs the tests and
builds of the changed workspace packages and their dependents, not the whole
JS workspace. WASM builds only when a package that consumes it is selected.

## Plan

Observed: one run spent 7 ms on the changed package's tests, about 50s on the
rest of the workspace's tests, and 49s compiling WASM. The dispatcher already
selects Rust crates by dependents; give JS the same selection, derived from the
workspace's own dependency graph rather than a hand-kept list. A changed file
that belongs to no workspace package (`js/common/`, `js/justfile`,
`test/tsconfig.json`, `drafts/`) selects every package, as today.
