# [S] TS timing checks survive a stalled PCR

## Goal

The TR 101 290 timing checks the TS importer reports (PAT and PMT
repetition, PTS intervals) keep running while a program's PCR stalls or goes
missing, for a single program and for programs that share a PID. An outage on
a stream with a stalled PCR is counted, not hidden, and a stalled PCR is
itself reported as an input fault.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7294](https://github.com/moq-dev/moq/pull/7294).

Milestone: ✅ m1. Not chosen: m2.

- [moq-dev/moq#7294](https://github.com/moq-dev/moq/pull/7294)'s overnight decision (2026-10-31, on the maintainer's standing
  authorization) merged shared-PID stats as is and filed this root-cause fix:
  a stalled owner's clock hides shared-PID timing errors
  ([review](https://github.com/moq-dev/moq/pull/7294#pullrequestreview-5709848438)),
  and a lone program with a stalled PCR already loses the same outage on main.
- Each program's `Health` (`rs/moq-mux/src/container/ts/health.rs:175`) times
  its tables, PCR, and PTS on its own PCR clock, passed in as `now` from the
  importer's `Liveness`. When the PCR stops, `now` stops, and no interval
  check fires. Fall back to a clock that keeps running, such as the multiplex
  clock (`mux_rate`) or arrival time, while the PCR is stalled, and report the
  stall through the PCR checks.
- Test single-program and shared-PID streams with a PCR that stops while
  tables and PES continue, and with all three stopping: the outage shows in
  the counters either way. Add the case to the TS guide's coverage table.

Public API: none expected (stats fields only if a new counter is needed).
Wire: none.

## Related

- [TS stats count a shared PID once](/quest/m1/ts-reliability/shared-pid-stats.md) - [moq-dev/moq#7294](https://github.com/moq-dev/moq/pull/7294), which made a shared PID behave like the single-program case
