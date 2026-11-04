# [XS] Re-measure the AAC browser budgets after the TS importer groups each PES

## Goal

Seven scheduled nightlies on `main`, all after the TS importer began publishing
each PES as one group, show the AAC `bursty` browser rows discarding about as
much audio a minute as the Opus `bursty` rows. `test/audio-quality/budgets.json`
carries the six AAC browser rows' ceilings re-measured from them, by the rule in
its note, and the note's sentence that the AAC rows predate the grouping is
dropped.

## Waiting

- moq-dev/moq: seven scheduled Nightly runs on `main` after
  [moq-dev/moq#7898](https://github.com/moq-dev/moq/pull/7898) merges. Find them
  with `gh run list --workflow nightly.yml --event schedule` and download each
  with `gh run download <id> -n audio-quality-run`; never dispatch the nightly by
  hand. The artifacts expire after 7 days, so download each run's as it lands
  rather than waiting for the seventh.

## Plan

[moq-dev/moq#7898](https://github.com/moq-dev/moq/pull/7898) re-measured every
browser row from the 2026-10-29 to 2026-11-04 nightlies (its body keeps each
night's metrics) and changed the publisher: `moq import ts` used to send each
seven-frame AAC PES as seven one-frame groups, which the transport delivers
newest first, so 82 to 86% of AAC frames arrived behind a newer one. Each PES is
now one group, its frames in presentation order.

On those nights, before the change, AAC `bursty` discarded up to 368.3 ms a
minute against Opus `bursty`'s 117.4. Replaying their captures with each PES's
arrival slots handed to its frames oldest first, as one group, halved the AAC
`bursty` and `wide` discards (worst `bursty` night 362.7 to 190.1 ms a minute)
and lowered both rows' target by 20 to 60 ms. That shallower target doubled AAC
`wide`'s player re-stalls (mean 3.0 to 6.6 a minute), which is the 0.95
quantile's trade and [Watch](/quest/m1/audio-jitter-target/watch.md)'s time
stretch to absorb, not a reason to deepen the target here.

Grade the same `chromium-aac-*` rows. If AAC `bursty` still discards several
times what Opus `bursty` does, compare the rows' `.arrivals.ndjson` captures:
with one group per PES nearly every frame should arrive in order.
