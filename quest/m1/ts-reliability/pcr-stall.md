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

The draft implements running arrival extrapolation, once-per-outage PCR counts,
shared-PID handover and EOF checks, with paused-time tests and a batching/fanout
benchmark. It is incomplete: review found that healthy batching is graded as an
arrival fault, and packets held by clock-step admission lose their original
arrival times and are graded before release. Resolve the policy below before
finishing that accounting. Keep the original arrival time with held input and
settle it before EOF grading. The draft's clean-feed CLI fixture was changed to
continuous smaller writes; revisit it under the selected policy.

Public API: none expected (stats fields only if a new counter is needed).
Wire: none.

## Waiting

- Maintainer: settle the clock policy exposed by local review of
  [moq-dev/moq#7727](https://github.com/moq-dev/moq/pull/7727).
  1. Preserve encoder-insertion grading on clean batched delivery (recommended):
     use a multiplex-clock fallback where measurable and settle the supported
     unmeasured/VBR fallback profile before completing the implementation.
  2. Accept arrival-based ingestion grading: upstream buffering and batched
     writes may raise the existing PCR, PAT, PMT and PTS error counters even
     when encoder insertion is clean.
  The Plan permits arrival fallback but does not explicitly settle this change
  in counter meaning. Check: record the choice on the draft PR, then finish
  held-packet arrival accounting and rerun checks and local review.
