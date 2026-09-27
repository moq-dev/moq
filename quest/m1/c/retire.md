# [S] Retire the hand-written libmoq

## Goal

The hand-written C crate is gone: its last release points users at the
generated `moq-c` 0.8.0, and the source, workflows, and docs that only served
it are deleted. `doc/setup/upgrade.md` tells C users how to move.

## Plan

- Fold in any retirement step #4288 already planned for the renamed crate.
- Decide the parked m3 libmoq quests: delete each whose outcome the generated
  API already provides, and say so in the PR.

## Required

- [C consumers](/quest/m1/c/consumers.md) - nothing in the repository still calls the hand-written ABI
