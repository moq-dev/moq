# [M] Share local check capacity across worktrees

## Goal

Expensive local check and test phases enter `quest gate` automatically, sharing
one host/user budget with other worktrees and repositories. One admitted suite
also bounds its own worker fanout so it does not overwhelm the desktop.

## Plan

- Decided 2026-10-15: gate expensive commands, not agent sessions. Put admission
  before expensive builds and fixture launch, outside test timeout clocks.
  Keep hosted CI's existing runner/merge-queue behavior.
- Audit root and direct language/test entrypoints together. Nested invocation
  must not deadlock or bypass the cap. Cheap selection and docs-only checks
  should remain cheap. Verify cancellation leaves no owned workload running.
- Reuse mbx for Rust compilation. Measure nextest thread/group limits and Bun
  workspace fanout for local runs; do not blindly enable mbx test scheduling
  where queued time would consume nextest's timeout budget.
- Benchmark representative solo and concurrent checkouts, recording wall time,
  CPU time, worker counts, cache state, and other host load. Compare total work
  separately from admission delay. Choose defaults from those measurements.
- Explain direct invocation and configuration in existing developer docs.
  Preserve selected tests and their failure behavior.

## Required

- [Pin the gate](/quest/m0/ci/quest-gate-pin.md) - the shared primitive is available in the devshell
