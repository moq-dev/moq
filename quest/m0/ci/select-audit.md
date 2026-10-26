# [S] Finish the CI selector's CPU and missed-edge audit

## Goal

Complete the package-selection audit with measured runner CPU cost and replay
full-run failures against the selected gate to find missed dependency edges.
The retrospective and its per-attempt evidence live in
[the CI selection report](/.github/ci-selection-audit.md).

## Plan

- The September 26 through October 25 audit classified 1,979 queue attempts:
  62 forced every module, 244 forced the Rust workspace, and 75 had Cargo.lock
  as their only shared-input trigger. Four historical attempts could not be
  classified.
- The audit fixed the missing `.cargo/` edge in Linux dispatch and Rust crate
  selection, with regression tests. Keep existing shared-input widening until
  CPU evidence justifies narrowing it.
- Archived logs contain elapsed durations, not runner CPU consumption. Decide
  whether to accept the retrospective's explicit measurement limit or collect
  process CPU measurements over a new observation window. The recommendation
  is to collect CPU time before adding lockfile-diff dependency resolution.
- Replay changes whose Nightly or `just gate` failed where the selected gate
  passed. Separate actual missed edges from checkout-history failures and
  feature-unification issues already owned by other quests or open PRs.
- kixelated/rust2's
  [MIR test selection](https://github.com/kixelated/rust2/blob/main/quest/m3/test-select.md)
  measures a finer selector against this one.

Public API: none.
