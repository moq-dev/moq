# [S] The land gate's just ci stops at the first failure

## Goal

When `quest land serve` checks a batch, `just ci` stops every lane as soon as
one module fails and reports that module, so a failing batch frees the gate
in seconds instead of running the remaining modules. Without the gate's
opt-in, `just ci` still runs every lane to the end.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from moq-dev/quest's host-gate cutover for moq.pro and rust2 and its fixer repairs).

- On 2026-10-31 two moq batches failed `check gh` in 2 seconds (`alert.yml`'s
  workflow list named the removed Pullfrog) and still ran 32-40 minutes each
  before ejecting.
- `sh/dispatch.sh` deliberately completes every lane "so one unavailable
  check cannot hide unrelated failures". Keep that default for local and
  nightly runs; add an opt-in the land check sets (an environment variable
  or `just ci` flag), and have kixelated/nixos's moq check command pass it.
- On the first failed module, signal the other lanes' process groups the way
  the existing timeout path does (`kill -TERM -- -$pid`), then print the
  seconds-per-module summary as today.
