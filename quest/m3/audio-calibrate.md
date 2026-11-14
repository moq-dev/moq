# [S] Re-measure every audio ceiling after the playout rule lands

## Goal

`test/audio-quality/budgets.json` carries every browser and native row's
ceilings re-measured, by the rule in its note, from seven scheduled Nightly
runs on a `main` that includes the whole
[audio playout](/quest/m2/audio-playout/README.md) epic. The rows budget
steady-state lag, the time audio takes to return to the clock after a
disturbance, the player's own trims and skips, and underruns as measured,
and the note says the rule is in them.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan) for the stretch model. This
replaced two waits that were calibrating a player the model changes: the
chromium re-measure after
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) and the native
one after [moq-dev/moq#8308](https://github.com/moq-dev/moq/pull/8308).

Moved 2026-11-13 (maintainer, /quest-plan) to m3 behind the epic: ✅ the cut
waits on the playout rule, not on a week of Nightlies to re-budget ceilings
that ship nothing (rejected: calibrating in m2 with the epic). Nights before
the epic's last child merges do not count; this supersedes the same day's
decision to wait only for the congestion fix.

Carried decisions:

- 2026-11-07 (maintainer, on #8058): ✅ land the player change, then measure
  (rejected: hold it until Nightly ceilings are in). The goal is judged by
  the rows alone, with no listening run.
- 2026-11-09 (maintainer, on
  [moq-dev/moq#8308](https://github.com/moq-dev/moq/pull/8308#issuecomment-6730322286)):
  ✅ budget `trimmed_ms_per_min` and `player_skips_per_min` (rejected:
  budgeting accelerates as discarded audio), and ✅ the nightly runner's
  playback benchmarks count as the quiet-host run (rejected: naming another
  host).

The rule: the worst of seven clean nights times 1.25, plus the documented
runner hiccup, rounded to two significant figures. A developer run is not
that calibration. Keep each night's rows in the PR body.

What to report rather than budget, since each means the rule is not doing its
job there: a steady-state `av_lag_ms` p50 more than a frame off zero on any
row; audio not back on the clock within about 2 s of a disturbance; native
Opus `step` still filing the 26 to 27 underruns a minute it has since #8507,
against 0 before.

Native rows stay report-only; enforcing them belongs to
[Audio outlives video under bufferbloat](/quest/m4/audio-bufferbloat-av.md).

## Waiting

- moq-dev/moq: seven scheduled Nightly runs on `main` after the epic's last
  child merges. Find them with `gh run list --workflow nightly.yml --event
  schedule` and download each with `gh run download <id> -n
  audio-quality-run` as it lands, since artifacts expire after 7 days. Never
  dispatch the nightly by hand.

## Required

- [Audio playout](/quest/m2/audio-playout/README.md) - the rule whose rows these ceilings budget
