# [S] Re-measure the chromium audio ceilings with the browser's time stretch

## Goal

`test/audio-quality/budgets.json` carries every chromium row's ceilings
re-measured, by the rule in its note, from seven scheduled Nightly runs on
`main` that all ran with the browser's NetEq time stretch, and budgets
`player_skips_per_min` on those rows beside the tap metrics. No chromium row
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
replay row exactly as measured, `relay-mic-rtt40` at 1.4
`player_underruns_per_min` (7.9 without the stretch), and left the browser
ceilings unchanged. Expect them to move both ways: stalled time and player
re-stalls should fall, while `skip_aheads` and `discarded_ms`, which infer a
skip from a step in the playhead's lag, read a sustained accelerate as a skip
and can rise on rows whose target moves. `player_skips` counts the ring's real
re-anchors, so a rise there is a regression worth reporting before budgeting
it. Rewrite the note's browser sentences to the new nights and say the stretch
is in them.

## Related

- [Capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md) - re-measures the AAC browser rows from nightlies too; the same seven nights can serve both
- [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) - the native rows' re-measure after the same stretch
