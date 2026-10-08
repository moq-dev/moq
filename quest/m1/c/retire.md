# [S] Retire the hand-written libmoq

## Goal

The hand-written C crate is gone: its final `moq-c` 0.7.x release points
users at the generated `moq-c` 0.8.0, and the source, workflows, and docs that only served
it are deleted. `doc/setup/upgrade.md` tells C users how to move.

## Plan

- #4288 renamed libmoq to `moq-c` and left a code-free `rs/libmoq` stub
  whose last release points at `moq-c`;
  [Retire the libmoq stub](/quest/m1/libmoq-retire.md) deletes that stub. This quest retires the
  hand-written `rs/moq-c` itself, so fold in or delete that quest, whichever
  is still open.

## Required

- [C consumers](/quest/m1/c/consumers.md) - nothing in the repository still calls the hand-written ABI

## Related

- [Retire the libmoq stub](/quest/m1/libmoq-retire.md) - deletes the `rs/libmoq` stub under the old name
