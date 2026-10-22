# [XS] One test filter per line in test/justfile

## Goal

Two PRs that each add an interop test filter or harness file to
`test/justfile` rarely conflict: they conflict only when both insert at the
same point of a sorted list, and that resolves by keeping both lines. Every
list on a single long line (the `interop` recipe's
`just rs test -p moq-net --lib ...` filters and the `harness` recipe's
browser test files) puts one entry per line, and the selected tests stay
exactly the same.

## Plan

Decided 2026-10-22 (maintainer): many PRs on 2026-10-22 conflicted on the
204-character filter line (`test/justfile:50`), for example
[moq-dev/moq#6158](https://github.com/moq-dev/moq/pull/6158),
[moq-dev/moq#6233](https://github.com/moq-dev/moq/pull/6233) and
[moq-dev/moq#6228](https://github.com/moq-dev/moq/pull/6228).
Rejected for now: MIGRATE.md entry fragments, and `merge=union` in
`.gitattributes`, which silently merges real conflicts.

- Use just's line continuation (`\`) or a recipe variable, whichever keeps
  `just --fmt --check` and the `gh`/justfile lints clean.
- Sort each list, so new entries land at different points instead of all at
  the end, and close it with a fixed line that holds no entry (the
  `--run-ignored all` flags, for example), so adding an entry never edits a
  neighbor's line to add or drop its continuation.
- Line layout alone cannot make two inserts at the same point merge cleanly;
  only one file per entry could, which costs more than the conflicts it
  saves.
- Keep the prose comment above the recipe, but stop it repeating the filter
  list, so a new filter edits one place.
- Prove nothing changed: compare the expanded commands (`just --dry-run`)
  before and after.

Public API: none. Wire: none.
