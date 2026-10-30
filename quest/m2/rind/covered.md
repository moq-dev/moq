# [XS] rind covers MoQ

## Goal

A rind release on crates.io carries everything rind's
[MoQ coverage epic](https://github.com/moq-dev/rind/blob/main/quest/m1/moq/README.md)
lists: the new primitives, the CPU check, armv7, and every `rind-*` adapter,
including `rind-instant-acme` once instant-acme 0.9 is out. Each adapter's
operations pass rind's tests on x86_64, aarch64, and armv7 alike; an operation
missing on one arch does not count as covered. It holds once that epic's
directory, `quest/m1/moq/` in moq-dev/rind, is gone and its release is on
crates.io. This quest only waits; delete it once that holds.

## Plan

One condition gates every area quest, because the migration waits for armv7
(maintainer, 2026-10-26).

Public API: none. Wire: none.
