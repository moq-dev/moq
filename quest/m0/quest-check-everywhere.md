# [XS] quest check runs on every branch that carries quests

## Goal

`quest check` fails any push or PR that breaks quest structure on `main`,
`dev`, and the questline branches, not only PRs into `main`.

## Plan

- `check.yml` runs `quest check` on pull requests only. Also run it on push to
  `main`, `dev`, and `quest/**`, so a direct merge commit (such as `main`
  merged into a line) can't land a broken tree. Use a dedicated job that runs
  `quest check` unconditionally: `check.yml`'s scope steps diff against
  `origin/$GITHUB_BASE_REF`, which is empty on a push.
- `dev` already pins main's `quest` (8590d2a) and passes since #4428. Only the
  wildcard line still pins 46d7fe8: merge `main` into it to bump the pin, and
  fix what the new check reports.

Public API: none. Wire: none.
