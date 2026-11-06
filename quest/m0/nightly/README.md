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
  [m1](/quest/m1/receive-windows-evicted.md) on 2026-11-04 (maintainer).

Decided 2026-11-05 (maintainer, /quest-plan): catch the next moq-net bench
break in Nightly's measured coverage only. Do not add a test-mode bench run
to the PR gate; the full run costs about 17 minutes per PR.

Public API: none. Wire: none.

## Required

- [Nightly's Miri job stays inside its time budget](/quest/m0/nightly/miri.md) - both Miri passes go green, with ARM timings inside the 60-minute job and 15-minute-per-test budgets
- [Nightly's macOS platform job passes](/quest/m0/nightly/macos.md) - `moq-audio`'s tests compile on macOS again and the websocket version test's timeout is fixed if it recurs
- [The OBS stop-drain test reads no gap](/quest/m0/nightly/obs-stop-drain.md) - OBS and OBS TSan read every delivered frame and the track's end after `Stop()`, with no gap
- [The JS resume model finds no missing groups](/quest/m0/nightly/resume-model.md) - the JS resume model property test hands every reader each group it is owed
