# [M] AAC under bursty delivery plays without a re-stall loop

## Goal

The browser player plays AAC under the `bursty` network profile without
repeatedly running dry and refilling: the nightly audio-quality rows'
`stalled_share`, skip-aheads, and discarded audio fall to what the Opus rows
on the same profile show, on both the isolated and the postMessage ring paths.

## Plan

Measured by [#5442](https://github.com/moq-dev/moq/pull/5442) over the 7
scheduled nightlies of 2026-10-07 to 2026-10-13: both AAC bursty rows stalled
19 to 43% of the window every night, with about 17 skip-aheads a minute and
about 2 s a minute of audio discarded. AAC wide stalled 6 to 12%. The
budgets carry these numbers as measured so the job is green; this quest fixes
the player and tightens them.

Start after #5442 merges: it uploads the nightly artifacts and carries the
measured budgets this quest tightens.

Decided 2026-10-13 (maintainer): a child of the audio jitter target line,
reproducible from the harness now, so Watch's manual proof runs after it.

Reproduce before fixing. Start from the nightly's `audio-quality-run`
artifacts (uploaded every run since #5442), then capture the case as a
replay trace so the failure runs deterministically under
`js/watch/src/audio/replay.ts` on a simulated clock. AAC's 1024-sample frames
(23.2 ms at 44.1 kHz) against Opus's 20 ms are the first suspect: the
estimator's frame term, the ring sizing, and the skip-ahead threshold each
see frame duration. Watch already records that a ring running dry re-stalls
and refills to the target; find why AAC under bursts keeps re-entering that
loop. Fix the layer that is wrong, including `doc/concept/audio-jitter.md`
and its conformance corpus if the estimator itself is at fault, mirrored in
`rs/moq-audio`.

Tighten the replay budget for the new trace and the AAC browser ceilings in
`test/audio-quality/budgets.json` from re-measured nightly runs, with the
reason in review.

Public API: none expected. Wire: none.

## Related

- [Watch](/quest/m1/audio-jitter-target/watch.md) - the manual browser proof that follows this fix
- [Opus isolated-ring target](/quest/m1/audio-jitter-target/opus-isolated-target.md) - the other defect the same measurement found
