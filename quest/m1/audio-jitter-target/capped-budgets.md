# [XS] Re-measure the AAC browser budgets after the TS importer groups each PES

## Goal

`test/audio-quality/budgets.json` carries the six AAC browser rows' ceilings
re-measured, by the rule in its note, from seven scheduled nightlies on `main`
that all ran after the TS importer began publishing each PES as one group, and
the note's sentence that the AAC rows predate the grouping is dropped. Nearly
every AAC frame on those nights arrives in order.

## Waiting

- moq-dev/moq: seven scheduled Nightly runs on `main` after
  [moq-dev/moq#7898](https://github.com/moq-dev/moq/pull/7898) merges. Find them
  with `gh run list --workflow nightly.yml --event schedule` and download each
  with `gh run download <id> -n audio-quality-run`; never dispatch the nightly by
  hand. The artifacts expire after 7 days, so download each run's as it lands
  rather than waiting for the seventh.

## Plan

Decided 2026-11-04 (maintainer, on
[moq-dev/moq#7898](https://github.com/moq-dev/moq/pull/7898)): land one group
per PES as is. The extra AAC `wide` re-stalls are for Watch's time stretch to
absorb, and this quest no longer expects AAC `wide` to stop running dry.
Options shown:

- ✅ Land as is
- Land only the budgets, holding the importer change until time stretch lands
- Keep the change and deepen AAC's target

[moq-dev/moq#7898](https://github.com/moq-dev/moq/pull/7898) re-measured every
browser row from the 2026-10-29 to 2026-11-04 nightlies (its body keeps each
night's metrics) and changed the publisher: `moq import ts` used to send each
seven-frame AAC PES as seven one-frame groups, which the transport delivers
newest first, so 82 to 86% of AAC frames arrived behind a newer one. Each PES is
now one group, its frames in presentation order.

On those nights, before the change, AAC `bursty` discarded up to 368.3 ms a
minute against Opus `bursty`'s 117.4. Replaying their captures with each PES's
arrival slots handed to its frames oldest first, as one group, halved the AAC
`bursty` and `wide` discards (`bursty` summed over the seven nights 1156 to 584
ms a minute) and lowered both rows' target by 20 to 60 ms. That shallower target doubled AAC
`wide`'s player re-stalls (mean 3.0 to 6.6 a minute), the 0.95 quantile's trade
that [Watch](/quest/m1/audio-jitter-target/watch.md)'s time stretch absorbs.

AAC `bursty` still discards about three times what Opus `bursty` does (584
against 174 summed), and parity is not expected: the harness keeps ffmpeg's
default PES packing on purpose, so AAC frames reach the viewer a median 210 ms
behind their media time on `bursty` against Opus's 41. Grade the same
`chromium-aac-*` rows, and check the in-order claim with the share of each row's
`.arrivals.ndjson` frames that arrive behind a newer one.
