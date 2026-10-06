# [S] An export failure is not the broadcast ending

## Goal

`moq export ts --linger` reports an export failure while the broadcast is
still live as a failure, instead of treating it as the broadcast ending and
waiting out the linger (`rs/moq-cli/src/subscribe.rs`).

## Plan

Reported by t0ms while grading #4645, who offered to do the CLI side.

The error alone can't tell the cases apart: a publisher drop or SIGKILL
usually surfaces as a track `Err` too, and that must keep lingering (see the
comment above the linger in `run_ts`). Classify by the broadcast's state
instead. If it is still announced and live, the export failed: report it and
fail the run as before. If it closed or was replaced, it ended, clean or not,
and the linger starts. The track error can arrive before the broadcast's close
does, so the check has to settle that race rather than read the state once.

A test drives each case: an export failure on a live broadcast fails the run,
a clean finish lingers, and a killed publisher still lingers and resumes when
the broadcast returns.

Public API: none. Wire: none.

## Related

- [#4645 report](https://github.com/moq-dev/moq/pull/4645#issuecomment-6013366214) - the motivating `missed a decode deadline` failure exists only on #4645's branch
