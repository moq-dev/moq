# [XS] The final libmoq release is the stub

## Goal

The newest `libmoq` on crates.io is the code-free stub that `main`'s
`rs/libmoq` holds, whose README points at `moq-c`. Since #4738 publishing runs
only from `release`, so the condition clears at the next `main`-to-`release`
cut, which must take `main`'s stub for `rs/libmoq` at a version above
crates.io's current `max_version`. `main`'s stub is at 0.6.12, below versions
crates.io already holds as full crates, so the cut bumps it or the stub never
publishes.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

As of 2026-10-25 the newest release is libmoq 0.6.18 (2026-10-23), still cut
from `release`'s full crate, not the stub; patch releases keep raising it. Check with
`curl -s https://crates.io/api/v1/crates/libmoq` for a newer `max_version`.
