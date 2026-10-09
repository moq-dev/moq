# [S] Retire the hand-written moq-c crate

## Goal

The hand-written C crate is gone: its final `moq-c` 0.7.x release points
users at the generated `moq-c` 0.8.0, and the source, workflows, and docs that only served
it are deleted. `doc/setup/upgrade.md` tells C users how to move.

## Plan

- #4288 renamed libmoq to `moq-c` (`rs/moq-c`); this quest deletes that
  hand-written crate. The code-free `rs/libmoq` stub left under the old name
  is [Retire the libmoq stub](/quest/m1/libmoq-retire.md)'s, separately.

## Required

- [C consumers](/quest/m1/c/consumers.md) - nothing in the repository still calls the hand-written ABI

## Related

- [Retire the libmoq stub](/quest/m1/libmoq-retire.md) - deletes the `rs/libmoq` stub under the old name
