# [S] Faster runners for the merge queue

## Goal

The merge queue runs on whichever runner gives the best wall time per dollar
once the pipeline is optimized: hosted runners with the R2 caches,
Blacksmith, or the maintainer's NixOS desktop as a self-hosted runner. The
choice is made from measurements of the optimized pipeline, and its cost per
merged PR is recorded.

## Plan

Decided 2026-10-19: pick the runner only after the optimization quests land,
so a faster runner doesn't pay for waste. This reopens the 2026-10-13
decision to drop self-hosted CI, conditionally: compare all three options
against the new baseline.

- Main PRs run no cloud CI, so the comparison is the queue's Linux gate, plus
  whatever nightly and release PRs need.
- A self-hosted runner on the desktop would run contributor-controlled code:
  a maintainer enqueues the PR, but the merge group still builds and runs
  what the contributor wrote. Treat every job as untrusted. The desktop is
  not a candidate until ephemeral NixOS VMs keep jobs away from its
  credentials, files and network, as the closed
  [#5218](https://github.com/moq-dev/moq/pull/5218) prepared.
- Blacksmith: compare its price per minute and its cache against hosted ARM
  runners plus the R2 caches.
- Measure queue wall time per entry and cost per merged PR on each candidate.

## Required

- [CI pipeline audit](/quest/m0/ci/audit.md) - removes the waste first
- [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) - the hosted baseline's cache
