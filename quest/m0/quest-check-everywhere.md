# [XS] quest check runs on every branch that carries quests

## Goal

`quest check` fails any push or PR that breaks quest structure on `main`,
`dev`, and the questline branches, not only PRs into `main`.

## Plan

- `dev` has no `quest` flake input and no `quest check` in its justfile;
  #4428's main-into-dev sync brings both. After it lands, convert the
  old-format quests on `dev` until `quest check` passes there.
- Line branches pin their own `quest` revision (the wildcard line pinned
  46d7fe8 against main's 8590d2a), so an old pin passes an old format.
  Merging `main` into each active line bumps the pin; do that for the lines
  that fail today and fix what the new check reports.
- `just ci check` runs `quest check` on pull requests only. Also run it on
  push to `main`, `dev`, and `quest/**`, so a direct merge commit (such as
  `main` merged into a line) can't land a broken tree. Use a dedicated job
  that runs `quest check` unconditionally: `check.yml`'s scope steps diff
  against `origin/$GITHUB_BASE_REF`, which is empty on a push.

Public API: none. Wire: none.

## Required

- #4428 merged to `dev`
