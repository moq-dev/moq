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

- [moq-dev/moq#7294](https://github.com/moq-dev/moq/pull/7294) (merged)'s overnight decision (2026-10-31, on the maintainer's standing
  authorization) merged shared-PID stats as is and filed this root-cause fix:
  a stalled owner's clock hides shared-PID timing errors
  ([review](https://github.com/moq-dev/moq/pull/7294#pullrequestreview-5709848438)),
  and a lone program with a stalled PCR already loses the same outage on main.
- Each program's `Health` (`rs/moq-mux/src/container/ts/health.rs:214` at be39188000) times
  its tables, PCR, and PTS on its own PCR clock, passed in as `now` from the
  importer's `Liveness`. When the PCR stops, `now` stops, and no interval
  check fires. Fall back to a clock that keeps running, such as the multiplex
  clock (`mux_rate`) or arrival time, while the PCR is stalled, and report the
  stall through the PCR checks. Today `health.tick` runs only on the
  PCR-bearing packet path and `Health::pcr` checks only when a PCR arrives,
  so advance the checks from every packet on the fallback clock. A fully
  stopped input has no packet to advance them: the next packet or EOF
  reports that outage, with no timer. Report a missing PCR once per outage;
  a returning PCR clears it so a later outage reports again.
- Test single-program and shared-PID streams with a PCR that stops while
  tables and PES continue, and with all three stopping: the outage shows in
  the counters either way. Add the case to the TS guide's coverage table.

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7727](https://github.com/moq-dev/moq/pull/7727)): grade insertion.

- ✅ Preserve insertion grading on clean batched delivery: use the multiplex
  clock where it is measurable, define the fallback for unmeasured or VBR input,
  and keep held input's arrival history. Not chosen: accept arrival-based
  ingestion grading, where upstream buffering and batched writes raise the
  counters on clean insertion.

This changes the fallback above: each program's health clock follows its PCR
and, across a stall, the packets the multiplex carries at the constant rate the
PCR measured. Without a measured rate (before the first 2 s window, on VBR
input, or after the PCR breaks) it falls back to arrival, applied once each
input chunk is read whole, and a chunk's own PCR, tables and PTS answer for its
delivery delay. A fully stopped input adds no time, by timer or at EOF: the
PCR's step when it resumes counts an encoder outage. Input held by clock-step
admission keeps the arrival of the chunk that completed each packet.

Public API: none expected (stats fields only if a new counter is needed).
Wire: none.

