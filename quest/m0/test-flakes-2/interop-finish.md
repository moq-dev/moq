# [S] Interop's finish cases hold up under load

## Goal

`just test interop --finish --all` passes every `finish <publisher> -> *` cell
on a loaded host, and the scenario's quarantine in `test/interop/interop.sh`'s
`run_finishes` is removed.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete), under the repository's
flaky-test rule, and widened the same day to every publisher. Local
`just test interop --all` runs under a host load average above 30 failed once
each and passed on rerun. No logs were kept.

- All nine `finish cpp -> *` cells failed together, while iterating
  [moq-dev/moq#8358](https://github.com/moq-dev/moq/pull/8358).
- The `finish rust -> *` cells failed, while iterating
  [moq-dev/moq#8220](https://github.com/moq-dev/moq/pull/8220): the Rust
  publisher never printed `finish acknowledged`, though every reader ended
  clean.
- The `finish go -> *` cells failed, while iterating
  [moq-dev/moq#8366](https://github.com/moq-dev/moq/pull/8366). The same run
  also failed `js -> c` and `rust-swapped -> js-native-node` once; those lanes
  are not quarantined, only recorded here as seen under the same load.

Suspected, not confirmed: with three publishers failing `finish` the same way,
the cause is shared, most likely the harness's own waits under load (the
publisher's wait for the finish acknowledgement, the FIFO the runner writes
after every reader ends, and the readers' deadlines), not one implementation.

Reproduce it first, under load as the epic describes, with the interop logs
kept, and check whether the two unquarantined lanes fail the same way. Fix the
cause, never with a longer limit, then remove the quarantine.

Public API: none. Wire: none.
