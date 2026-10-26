# [S] Audit CI's package selection

## Goal

A report on how well `just ci`'s selection (`sh/dispatch.sh`, `sh/changed.sh`,
and Rust's crate selection in `sh/rs/select.sh`) narrows the merge queue's work, from the last month of queue runs: how often an
entry builds and tests every package, which paths forced it (toolchain, lockfile,
`flake.nix`, CI files, root manifests), and whether each of those rules is
needed. It fixes any rule that selects too much, or too little, with a test in
`sh/changed.test.py`, `sh/rs/select.test.py`, or their neighbors.

## Plan

Decided with the maintainer on 2026-10-25: moq already builds and tests only the
packages a branch changed plus their dependents, so check that selection's rules
rather than adding another. Measure in runner CPU time.

- A rule that selects too little lets a breaking change through the queue; a
  missed dependency edge counts as a bug, found by replaying changes whose full
  run (Nightly or `just gate`) failed where the selected run passed.
- A `Cargo.lock` bump that touches one dependency only needs the packages that
  use it. Say whether selecting from the lockfile diff is worth its complexity.
- kixelated/rust2's
  [MIR test selection](https://github.com/kixelated/rust2/blob/main/quest/m3/test-select.md)
  measures a finer selector against this one.

Public API: none.
