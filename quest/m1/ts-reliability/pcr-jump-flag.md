# [S] The TS export flags a PCR only where its clock jumps

## Goal

`moq export ts` sets `discontinuity_indicator` on a PCR exactly when the
time base it asserts breaks: a declared switch, as today, or a schedule that
skips empty slots so the PCR jumps past the bytes between. A delivery skip the
schedule pads on a continuing clock goes out unflagged, a gap in the programme
and not a new time base.

## Plan

Implementation is in [moq-dev/moq#7981](https://github.com/moq-dev/moq/pull/7981).
The scheduler flags actual skipped slots, continuing-clock padding stays unflagged,
and obsolete delivery-loss flag state is removed. Tests and docs match that behavior.
Finish validation, then delete this quest and its link when the PR is ready.

## Waiting

- moq-dev/moq: `just test interop --all` exits 2 because `MOQ_QUEST` is rejected
  as an unknown environment variable. [moq-dev/moq#7979](https://github.com/moq-dev/moq/pull/7979)
  fixes the dev-shell collision. Recommended: land that tooling fix, merge main
  into this branch, and rerun the unchanged interop harness.

## Closes

- [#7965](https://github.com/moq-dev/moq/issues/7965) - `export ts`: the skip #7741 reported keeps its clock
