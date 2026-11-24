# [S] JS: a range added below a lite-07 start learns the stale run's fate

## Goal

`js/net`'s lite-07 publisher matches Rust's `TrackRun::lower_start`: a stale run it took back unnamed below where delivery started gets its verdict once an update adds a range over it. A front reader's take-back also reaches its copies. Rust and JS match.

## Plan

- Found on [moq-dev/moq#9538](https://github.com/moq-dev/moq/pull/9538), which fixed the Rust side. `js/net/src/lite/publisher.ts` lowers `startKey` when a lower range is added, but leaves the stale runs it forgot below the old start unreopened. `js/net/src/track.ts` clears only the logical report, not the routed copies'.
- Port the remembered taken-back runs. Reopen only the parts the wire was still not told, because a verdict forwarded since the take-back is final. Make the take-back and the reopen reach the routed copies.
- Port both Rust tests: `lite07_a_range_added_below_the_start_learns_the_stale_runs_fate` and `a_run_taken_back_reports_the_copys_later_drop`.

Public API: none. Wire: none.
