# [M] Weekly mutation testing on critical modules

## Goal

A weekly scheduled job runs cargo-mutants on `moq-auth` and on `moq-net`'s
path permission, resume, and expiry code, and reports surviving mutants,
so assertions that never notice a behavior change get found.

## Plan

From the 2026-10-27 coverage audit. The failure drills already use
mutation-style negative controls; nothing does it systematically.

Decided 2026-10-27 (maintainer):

- Weekly, not nightly: the scoped code changes slowly and each mutant
  rebuilds and reruns its crate's tests.
- Scope to the modules above with cargo-mutants' file filters, and bound each
  mutant with its timeout so one hung mutant cannot eat the job.
- Survivors go to the job summary, and a run with survivors alerts through
  `alert.yml`.
- No mutation-score gate. The PR that adds the job fixes or annotates the
  first run's survivors: add a test where one is worth killing, or exclude the
  mutant in cargo-mutants' config with the reason (an equivalent mutant, or
  logging only).

Public API: none. Wire: none.
