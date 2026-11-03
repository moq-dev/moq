# [XS] A jq error in the Rust selection fails the run

## Goal

`just check`'s Rust selection fails the run when one of its jq filters
errors, instead of logging the error and selecting less.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7303](https://github.com/moq-dev/moq/pull/7303), [moq-dev/moq#7280](https://github.com/moq-dev/moq/pull/7280), [moq-dev/moq#7304](https://github.com/moq-dev/moq/pull/7304), and [moq-dev/moq#6946](https://github.com/moq-dev/moq/pull/6946). Milestone: ✅ m0 (user, "all recommended" for quests 1-8).

The other two parts of the original quest are gone from main:
[moq-dev/moq#7473](https://github.com/moq-dev/moq/pull/7473) (merged) moved
`just rs send-depth` out of the gate into Nightly's Tests matrix
(`.github/workflows/nightly.yml:355`), which runs it whatever changed, and
[moq-dev/moq#7475](https://github.com/moq-dev/moq/pull/7475) (merged)
deleted `test/wasm/uniffi/` and `wasm-baseline` with the rest of WASM.

- **The noise**: #7303's `just check` logged `jq: error ... null cannot be
  matched` without failing. That message matches what
  `sh/rs/uring.test.py` (run by `.github/justfile:72`) printed four times
  from a cargo-metadata fixture without `workspace_root` or `targets`;
  [moq-dev/moq#7621](https://github.com/moq-dev/moq/pull/7621) (merged)
  completed the fixture and asserts empty stderr. Confirm a `just check` on
  current main logs no jq error; if one remains, find and fix its filter.
- **Failing the run**: `sh/rs/select.sh:9` sets `set -euo pipefail`, but
  `select_packages` runs as `packages=$(select_packages)` (`:112`, at
  be39188000), and bash clears `-e` inside a command substitution, so a jq
  failure in its filters (`:38`, `:64`, `:77`, `:104`) lets the function
  carry on and print a smaller selection. #7621's "before" run shows it:
  io_uring selection passed with four jq errors. Make the failure propagate
  (`shopt -s inherit_errexit`, or checking each `jq` status), check
  `sh/rs/c-tests.sh:36` for the same pattern, and add a
  `sh/rs/select.test.py` case where malformed metadata fails selection.

Public API: none. Wire: none.
