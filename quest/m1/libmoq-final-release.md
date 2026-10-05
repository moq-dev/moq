# [XS] The final libmoq release is the stub

## Goal

The newest `libmoq` on crates.io is the code-free stub that `main`'s
`rs/libmoq` holds, whose README points at `moq-c`. Since #4738 publishing runs
only from `release`, so the condition clears at the next `main`-to-`release`
cut, which must take `main`'s stub for `rs/libmoq`.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

As of 2026-10-05 the newest release is libmoq 0.6.10 (2026-10-03), again cut
from `release`'s full crate, not the stub, and the open release PR #4763
bumps `release`'s full libmoq once more. Check with
`curl -s https://crates.io/api/v1/crates/libmoq` for a newer `max_version`.
