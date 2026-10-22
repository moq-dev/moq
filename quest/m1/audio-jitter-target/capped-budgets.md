# [XS] Re-measure the browser budgets after the estimator capped one observation's weight

## Goal

Seven scheduled nightlies on `main` after the estimator began capping how much
weight one observation takes show the AAC `bursty` browser rows discarding
about as much audio a minute as the Opus `bursty` rows, on both rings, and
`test/audio-quality/budgets.json` carries every browser row's ceilings
re-measured from them.

## Plan

Waits on the nightlies: check the `audio-quality-run` artifacts of the
scheduled Nightly runs on `main` (`gh run download <id> -n audio-quality-run`)
once seven have run since the change merged. Never dispatch the nightly by
hand. Re-measure by the rule in the budgets note, and drop the note's sentence
that budgets the AAC bursty discards against Opus's.

The cap changed every row's startup, not only AAC bursty's: the prior is
replaced in about five seconds instead of two, and a late interval in those
first seconds holds the target for up to one 20 s half-life instead of falling
back within seconds. Replays of local captures held the clean rows' targets
where they were, but one live Opus `near-zero` `plain` run held a startup
hiccup at 160 ms for 20 s, over that row's 95 ms ceiling. If a nightly breaches
a startup target ceiling that way, budget it as measured rather than reopening
the estimator.

On replays of the 2026-10-18 nightly capture and of two local captures, the
cap cut AAC bursty discards from 540 to 860 ms a minute to 0 to 170, and its
target p95 from 663 ms to 343 to 363. If the nightlies still show AAC bursty
discarding several times Opus's, the remaining source is the publisher: each
seven-frame PES flush is seven one-frame groups, delivered newest first, so the
oldest frame of a flush waits behind every newer group on a path that cannot
carry them together.

## Related

- [Opus near-zero budgets](/quest/m1/audio-jitter-target/opus-near-zero-budgets.md) - re-measures some of the same rows from nightlies; take them after the cap so one pass covers both
