# [M] Cut the AAC bursty discarded audio to the Opus level

## Goal

The AAC `bursty` browser rows discard about as much audio a minute as the Opus
`bursty` rows do on the same nightlies, on both rings, and
`test/audio-quality/budgets.json` carries AAC bursty ceilings re-measured from
seven scheduled nightlies on `main` after the fix.

## Plan

Split from the AAC bursty budgets re-measure
([#6128](https://github.com/moq-dev/moq/pull/6128)), which met that quest's
stall and skip-ahead goal but not its discarded-audio goal. On the seven
nightlies from 2026-10-14 to 2026-10-20, AAC bursty discarded up to 873 ms a
minute isolated and 687 plain, against 156 and 208 for Opus bursty, while its
`stalled_share` and skip-aheads sat at Opus's level.

The `bursty-aac` replay trace is the 2026-10-18 AAC bursty isolated capture,
which holds every group the player saw, and its replay rows reproduce the live
row (857 ms discarded a minute against 873 live). It shows two late-flush
episodes: each lifts the target, which decays back to 343 ms within about 5 s,
so the next late flush re-stalls. Reproduce and fix from that replay
(`just test audio-quality --replays`), not the nightly. A fix to the estimator
changes `doc/concept/audio-jitter.md` and both implementations together.

Then re-measure the AAC bursty rows by the rule in the budgets note, waiting on
seven scheduled nightlies after the fix merges. Never dispatch the nightly by
hand.
