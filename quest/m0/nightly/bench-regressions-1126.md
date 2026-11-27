# [M] Nightly 2026-11-26's paired benchmark regressions are fixed or accepted

## Goal

Each regression Nightly [45867457463](https://github.com/moq-dev/moq/actions/runs/45867457463)
confirmed (`4fba5df73f` against `420b8dcb0d`) is bisected to its PR and either fixed or
accepted with the feature it pays for, recorded in this epic's README the way the
`routes/lagging` cost was:

- `moq-audio/capture` `capture_fanout/*`: all six cells +14.1% to +16.9%
  (`Nightly (Rust supplemental benchmarks)`).
- `moq-net/session`
  `session_delivery_group_cost_cpu/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=1/watch=1/frame=64`:
  +11.2% (`Nightly (Rust bench targets)`).

## Plan

Decided 2026-11-26 (maintainer, /quest-plan): ✅ a bisect quest in m0 (rejected: add to
[perf-recheck](/quest/m3/release/perf-recheck.md)).

No moq-audio source changed in the range, so the capture regression comes from a dependency:
the likeliest is [moq-dev/moq#9675](https://github.com/moq-dev/moq/pull/9675), which moved
`rs/kio` to the external kixel-ai/kio crate (check inlining and codegen across the new crate
boundary). The lite-07 group cost likeliest comes from
[moq-dev/moq#9686](https://github.com/moq-dev/moq/pull/9686), which settles starts through
GROUP_END and SUBSCRIBE_DROP(SKIP).

Measure in alternating pairs on one pinned core, with instruction counts, as the
`routes/lagging` triage did. If a regression doesn't reproduce locally, re-check the next
Nightly before bisecting.

Public API: none. Wire: none.
