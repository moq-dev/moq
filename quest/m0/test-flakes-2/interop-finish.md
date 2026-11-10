# [S] Interop's cpp and rust finish cases hold up under load

## Goal

`just test interop --finish --all` passes its `finish cpp -> *` and
`finish rust -> *` cells on a loaded host, and their quarantine in
`test/interop/interop.sh`'s `run_finishes` is removed.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete), under the repository's
flaky-test rule. Two agents' local `just test interop --all` runs, both under a
host load average above 30, failed once each and passed on rerun. No logs were
kept.

- All nine `finish cpp -> *` cells failed together, while iterating
  [moq-dev/moq#8358](https://github.com/moq-dev/moq/pull/8358).
- The `finish rust -> *` cells failed, while iterating
  [moq-dev/moq#8220](https://github.com/moq-dev/moq/pull/8220): the Rust
  publisher never printed `finish acknowledged`, though every reader ended
  clean.

Reproduce it first, under load as the epic describes, with the interop logs
kept. Suspected, not confirmed: the publisher's wait for the finish
acknowledgement (the FIFO the runner writes after every reader ends). That
cpp's cells all failed together also points at its publisher rather than any
reader. Fix the cause, never with a longer limit, then remove the quarantine.

Public API: none. Wire: none.
