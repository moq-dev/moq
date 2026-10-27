# [XS] rind covers MoQ

## Goal

A rind release on crates.io carries everything
[the bootstrap](/quest/m2/rind/bootstrap.md) lists for MoQ: the new primitives,
the CPU check, armv7, and every `rind-*` adapter, including
`rind-instant-acme` once instant-acme 0.9 is out. Each adapter's operations
pass rind's tests on x86_64, aarch64, and armv7 alike; an operation missing on
one arch does not count as covered. Check rind's `quest/` tree
for remaining MoQ-coverage quests and crates.io for the release. This quest
only waits; delete it once that holds.

## Plan

One condition gates every area quest, because the migration waits for armv7
(maintainer, 2026-10-26).

Public API: none. Wire: none.

## Related

- [Bootstrap rind](/quest/m2/rind/bootstrap.md) - creates the repo and the quests this waits on
