# [M] Nightly passes on main

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

Decided 2026-10-27 (maintainer), from the coverage audit's triage of run
[40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673): more
children for the causes nothing owned.

This README's own work, after the children:

- Confirm a scheduled run on `main` passes; never dispatch Nightly. Failures
  already owned elsewhere stay there: the audio near-zero p95 row belongs to
  [audio jitter target](/quest/m1/audio-jitter-target/README.md), the
  macOS `receive_credit` timeout has no owner since its quest was deleted in
  [moq-dev/moq#6732](https://github.com/moq-dev/moq/pull/6732), so plan one under
  [test flakes](/quest/m0/test-flakes-2/README.md) if it recurs, and the paired bench
  threshold is calibrated in [Paired comparison](/bench/README.md#paired-comparison). The moq-net benchmark
  fixtures were repaired in [moq-dev/moq#7940](https://github.com/moq-dev/moq/pull/7940).
  The receive-windows benchmark's overcommitted cells, skipped since
  [moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838), moved to
  [m1](/quest/m3/receive-windows-evicted.md) on 2026-11-04 (maintainer).

Decided 2026-11-05 (maintainer, /quest-plan): catch the next moq-net bench
break in Nightly's measured coverage only. Do not add a test-mode bench run
to the PR gate; the full run costs about 17 minutes per PR.

Public API: none. Wire: none.

Decided 2026-11-09 (maintainer, /quest-complete), from the triage of runs
[42377867523](https://github.com/moq-dev/moq/actions/runs/42377867523) and
[42487375545](https://github.com/moq-dev/moq/actions/runs/42487375545): four
more children for the causes nothing owned. Dropped: one-night bench moves
near the paired threshold (re-check if one repeats), a one-off
`remote error: 52` in the browser bench smoke, and failures main or
[moq-dev/moq#8290](https://github.com/moq-dev/moq/pull/8290) already fixes.
