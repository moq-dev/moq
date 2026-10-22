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

Measured by the CI pipeline audit (2026-10-21): since 2026-10-20 the queue
runs on the persistent `nix/runner` VM (`MOQ_QUEUE_RUNNER`, 8 cores) with
mbx's local store, so `target/` persists and a checkout rewrites only the
changed files. A `moq-quic` change recompiled only its dependents. Over 32
one-pass entries, the job took a median 5.5 minutes, p75 8.5, p90 11.9, and
at most 24.2 (every module selected), against 21 minutes for queue run
[39471110155](https://github.com/moq-dev/moq/actions/runs/39471110155) on a
hosted runner before the one pass. Queue waits fell to seconds once Check and
Test merged into one job. The job summary now lists seconds per module, and
the Rust test builds upload Cargo's `--timings` reports.

## Required

- [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) - the hosted baseline's cache
