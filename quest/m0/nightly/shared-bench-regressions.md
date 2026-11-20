# [M] The 2026-11-19 shared-bench regressions are fixed or accepted

## Goal

Nightly's Rust shared bench targets pass the paired comparison again: each
regression from 2026-11-19 is fixed, or accepted by the PR that caused it
with a stated reason.

## Plan

Found by the 2026-11-19 triage of run [44562804083](https://github.com/moq-dev/moq/actions/runs/44562804083),
against the 2026-11-18 head (`4f3ba3317`). Twelve regressions, consistent
across all three rounds:

- `connection_blocked/*/churn_*` +22 to 33%. Suspect
  [moq-dev/moq#9152](https://github.com/moq-dev/moq/pull/9152), which also
  changed that bench; check whether the bench now measures more, which is
  not a regression.
- `stats/presence/*` +38 to 56%. Suspect
  [moq-dev/moq#9054](https://github.com/moq-dev/moq/pull/9054), which adds
  `poll_refresh` per tick.
- `relay_auth_refresh/1/1` +11.3%.

Confirm each suspect with a paired local run of the bench at its PR and its
parent before changing code.

Caused by: the suspects above, once confirmed.

Public API: none expected. Wire: none.
