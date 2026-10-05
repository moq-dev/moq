# [M] moq adopts flat questlines from the current quest CLI

## Goal

moq pins a quest release where questlines are planning groups with no branch
of their own: every quest, including a questline README's remaining work, is
its own PR against `main`. The child work already merged into questline
branches reaches `main`, and those branches retire.

## Plan

kixelated/quest#44 dropped questline branches, `quest branch`, and
`quest ready --remote`; kixelated/quest#51 changed the import skill to treat a
report's fix as a claim and plan each issue fully. moq still pins the quest
input in `flake.nix` from before both.

Decided (2026-10-04): a quest of its own, not a routine pin bump, because 16
questline branches carry merged child work that is not on `main`
(`git ls-remote origin 'refs/heads/quest/*README'`).

- Bump the `quest` input and run `quest check` with the new CLI; fix what it
  reports.
- For each questline branch, land its accumulated work on `main` through its
  existing umbrella PR, or a new one, then delete the branch. A branch whose
  work cannot land yet is decided with the maintainer, not left orphaned.
- Add the `quest-takeover` skill stub beside the other `.claude/skills/quest-*`
  stubs, and decide with the maintainer whether it replaces the existing
  `takeover` skill.
- Update `AGENTS.md` and `CONTRIBUTING.md` only where they describe questline
  branches, with the maintainer's approval, as both files require.

Done when `flake.nix` pins the new quest, no `quest/*README` branch remains,
and `quest check` passes.

## Related

- [Merge queue](/quest/m1/merge-queue.md) - the other trunk workflow change
