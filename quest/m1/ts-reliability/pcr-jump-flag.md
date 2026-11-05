# [S] The TS export flags a PCR only where its clock jumps

## Goal

`moq export ts` sets `discontinuity_indicator` on a PCR exactly when the
time base it asserts breaks: a declared switch, as today, or a schedule that
skips empty slots so the PCR jumps past the bytes between. A delivery skip the
schedule pads on a continuing clock goes out unflagged, a gap in the programme
and not a new time base.

## Plan

Reported in [moq-dev/moq#7965](https://github.com/moq-dev/moq/issues/7965),
a correction to [moq-dev/moq#7741](https://github.com/moq-dev/moq/issues/7741):
in a live relay-kill run the export padded a 7 s outage with nulls on an
unbroken 25 ms PCR grid, and every PID's DTS kept its offset to the PCR. So
[moq-dev/moq#7785](https://github.com/moq-dev/moq/pull/7785), which flags
every delivery skip, declared a new time base equal to the old one there.
Verified against `main`: the claim holds for holes the schedule pads, but
`Schedule::index` (`rs/moq-mux/src/container/ts/schedule.rs`) skips the
empty slots of a gap longer than `BACKFILL` (1 s), and that PCR does jump.
`a_skipped_group_flags_the_clock` runs 6 s holes through that path.

Decided 2026-11-05 (maintainer, /quest-import of #7965):

- ✅ Flag real jumps only: derive the flag from the schedule, set on the first
  PCR after it skips slots, whatever caused the gap, so a source stall with
  no delivery skip is flagged too. Delete #7785's per-track loss tracking
  (`Track::{gap, last_timestamp, lost}`, `coalesce`, `Queued::gap`,
  `PesUnit::skipped`, `Schedule::{gap, skip}`), and the 2026-11-05
  coalescing decision in the epic README goes with it. Not chosen: keeping
  the flag as a skip marker and rewording its docs (it misstates the time
  base); reverting #7785 (a hole past the backfill would again carry an
  unflagged PCR jump); won't fix.
- ✅ m1, in the TS reliability epic.

Rework the tests to match: holes the schedule pads carry no flag and keep the
PCR's repetition health clean; holes past the backfill carry one flag each on
the first PCR after the jump. Fix `doc/bin/cli.md` and `test/ts/README.md`,
which describe the skip flag.

## Closes

- [#7965](https://github.com/moq-dev/moq/issues/7965) - `export ts`: the skip #7741 reported keeps its clock
