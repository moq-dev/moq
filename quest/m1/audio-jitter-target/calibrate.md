# [S] Re-measure every audio ceiling after the stretch model lands

## Goal

`test/audio-quality/budgets.json` carries every browser and native row's
ceilings re-measured, by the rule in its note, from seven scheduled Nightly
runs on a `main` that includes
[the stretch model](https://github.com/moq-dev/moq/pull/8507). The rows budget
steady-state lag, the player's own trims and skips, and underruns as
measured, and the note says the model is in them.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan). This replaces two waits that
were calibrating a player the model changes: the chromium re-measure after
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) and the native
one after [moq-dev/moq#8308](https://github.com/moq-dev/moq/pull/8308). Nights
before the model merges do not count.

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

What to report rather than budget: a steady-state `av_lag_ms` p50 more than a
frame above zero on any row but `step`, or a nonzero `player_skips_per_min`
on a row whose stalls fit inside the `stretch` bound. Either means the model
is not doing its job there.

Watch native-opus near-zero: at [moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507)'s
head a developer host at load average about 43 filed 64 to 77 tap underruns
a minute against its ceiling of 42, where two earlier runs filed 0. Decided
2026-11-11 (maintainer, /quest-merge pick of #8507): ✅ merge and treat it as
host noise for these nightlies to settle (rejected: re-measure on a quiet
host first; investigate #8507's review fixes first). If the nightlies
reproduce it, report it rather than budget it.

Decided 2026-11-13 (maintainer, /quest-plan): ✅ count only nights after
[the congestion fix](/quest/m1/audio-jitter-target/congestion-catch-up.md)
lands too, so the congestion rows budget the fixed player (rejected:
calibrate the other rows now and re-measure congestion later).

Native rows stay report-only; enforcing them belongs to
[Audio outlives video under bufferbloat](/quest/m4/audio-bufferbloat-av.md).

## Waiting

- moq-dev/moq: seven scheduled Nightly runs on `main` after the stretch model
  and the congestion fix merge. Find them with `gh run list --workflow nightly.yml --event
  schedule` and download each with `gh run download <id> -n
  audio-quality-run` as it lands, since artifacts expire after 7 days. Never
  dispatch the nightly by hand.

## Required

- [Congestion catch-up](/quest/m1/audio-jitter-target/congestion-catch-up.md) - the congestion behavior the ceilings budget
