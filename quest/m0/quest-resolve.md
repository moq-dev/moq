# [XS] Adopt the quest merge driver

## Goal

Local merges in this repository resolve concurrent quest link-list edits
without conflict markers. Waits on kixelated/quest shipping `quest resolve`,
planned in [kixelated/quest#199](https://github.com/kixelated/quest/pull/199):
check whether `quest resolve --help` works on kixelated/quest `main`.

## Plan

Decided 2026-10-23 (maintainer), from a replay of 280 merges of `main` into
PR branches: 29 of 128 conflicted merges conflicted only in quest files,
mostly `quest/m1/README.md`'s Required list. Git's `merge=union` was rejected
because it resurrects lines for quests `main` had deleted.

Bump the `quest` input in `flake.nix` to a revision with `quest resolve`, then
run `quest init` and commit the `.gitattributes` line it writes. Other clones
need no setup: any `quest` command registers `merge.quest.driver` in a clone
whose `.gitattributes` names it, and the dev shell puts `quest` on PATH. GitHub's own
merges ignore drivers, so a PR still shows as conflicting until a local merge
from `main` is pushed; that merge then resolves itself.
