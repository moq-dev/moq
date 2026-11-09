# [S] Re-measure the chromium audio ceilings with the browser's time stretch

## Goal

`test/audio-quality/budgets.json` carries every chromium row's ceilings
re-measured, by the rule in its note, from seven scheduled Nightly runs on
`main` that all ran with the browser's NetEq time stretch, and budgets
`player_skips_per_min` and `av_lag_ms` on those rows beside the tap metrics. No chromium row
skips buffered audio in steady state: its `player_skips_per_min` stays at the
startup re-anchors the estimator's hold releases, as on the replay rows. What
still runs dry is what NetEq would conceal, a receive stall longer than the
target, and its underrun ceilings are measured rather than fixed at zero.

## Waiting

- moq-dev/moq: seven scheduled Nightly runs on `main` after
  [moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) merges. Find them
  with `gh run list --workflow nightly.yml --event schedule` and download each
  with `gh run download <id> -n audio-quality-run`; never dispatch the nightly by
  hand. The artifacts expire after 7 days, so download each run's as it lands
  rather than waiting for the seventh.

## Plan

Decided 2026-11-07 (maintainer, /quest-complete on
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058)): ✅ land the
stretch, then measure (rejected: hold the PR until the Nightly ceilings are in).
The chromium rows can only be measured once the stretch is on `main`, so this
quest carries that measurement, and its Goal is judged by it and the replay
rows alone, with no maintainer listening run (decided 2026-11-07,
/quest-unblock).

[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) budgeted every
replay row exactly as measured, A/V lag included, and left the browser
ceilings unchanged. The browser's stretch never slows down, so underruns stay
where the trim had them (`relay-mic-rtt40` 10.6 `player_underruns_per_min`
against 8.2) while the audio the trim dropped plays faster instead, and the
A/V lag rests about one frame ahead of the clock. Expect the chromium rows
alike: re-stalls about as before, `player_skips` near zero, and
`skip_aheads` and `discarded_ms`, which infer a skip from a step in the
playhead's lag, reading a fast accelerate after a receive stall as a skip. `player_skips` counts the ring's real
re-anchors, so a rise there is a regression worth reporting before budgeting
it, as is an `av_lag_ms` p50 above zero, which is audio settling behind the
clock. Rewrite the note's browser sentences to the new nights and say the stretch
is in them.

[moq-dev/moq#8308](https://github.com/moq-dev/moq/pull/8308) adds the
player's own `trimmed_ms` and `accelerated_ms` to every row. The native
calibration budgets `trimmed_ms_per_min` beside `player_skips_per_min`, as the
maintainer decided there; budgeting it here too keeps the two runtimes graded
alike.

## Related

- [Capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md) - re-measures the AAC browser rows from nightlies too; the same seven nights can serve both
- [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) - the native rows' re-measure after the same stretch
