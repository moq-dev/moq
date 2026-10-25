# Nightly passes on main

## Goal

The scheduled Nightly on `main` passes end to end, so a red run means a new
break instead of noise. Nightly is the backstop for what the PR gate skips on
purpose (benches, examples, feature shapes; see
[moq-dev/moq#5531](https://github.com/moq-dev/moq/pull/5531)), and it has
failed every night since at least 2026-10-19, which let a bench and two
example breaks reach `main` unnoticed.

## Plan

Decided 2026-10-24 (maintainer), from a triage of run
[40090959108](https://github.com/moq-dev/moq/actions/runs/40090959108): one
child per distinct cause, each landing on its own. The PR gate keeps
skipping benches and examples; a green Nightly catches them within a day.

This README's own work, after the children:

- Nightly's multi-step jobs (Features first) keep going past a failed step
  and fail at the end, so one break never hides the steps after it.
- Confirm a scheduled run passes. Failures already owned elsewhere stay
  there: the audio near-zero p95 row belongs to
  [audio jitter target](/quest/m1/audio-jitter-target/README.md), the
  macOS `receive_credit` timeout to
  [test flakes](/quest/m1/test-flakes-2/README.md), and the paired bench
  threshold to [bench CI](/quest/m1/bench-ci.md). The `cli-export`
  bufferbloat failure is expected to clear with
  [moq-dev/moq#6656](https://github.com/moq-dev/moq/pull/6656).

Public API: none. Wire: none.

## Required

- [Feature shapes in the gate](/quest/m0/nightly/feature-shapes.md) - feature-gated dead code fails the PR that adds it, not the next Nightly
- [ts_schedule regression](/quest/m0/nightly/ts-schedule.md) - the moq-mux `ts_schedule` benches return to their old cost
- [Nightly's base in a shallow checkout](/quest/m0/nightly/shallow-base.md) - `sh/gh/nightly.test.py` passes in Nightly's depth-1 checkout
- [Advisory bumps](/quest/m0/nightly/advisories.md) - both audits pass on today's advisories
- [moq-net builds without async](/quest/m0/nightly/net-async-free.md) - the no-default-features lane stops pulling `async` back in
