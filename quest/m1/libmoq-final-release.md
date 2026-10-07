# [XS] The final libmoq release is the stub

## Goal

The newest `libmoq` on crates.io is the code-free stub that `main`'s
`rs/libmoq` holds, whose README points at `moq-c`. Since #4738 publishing runs
only from `release`, so the condition clears at the next `main`-to-`release`
cut, which must take `main`'s stub for `rs/libmoq` at a version above
0.6.12. `main`'s stub is at 0.6.11, which crates.io already holds as a full
crate, so the cut bumps it or the stub never publishes.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

As of 2026-10-06 the newest release is libmoq 0.6.12 (2026-10-06), still cut
from `release`'s full crate, not the stub; release PR #4763 merged and
published 0.6.11 the same way. Check with
`curl -s https://crates.io/api/v1/crates/libmoq` for a newer `max_version`.
