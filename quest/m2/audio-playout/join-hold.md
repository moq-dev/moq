# [M] The join's backlog does not hold the target for 20 s

## Goal

A subscription's opening backlog no longer holds the playout target, and
video's delay with it, above the steady state for up to a 20 s half-life
after the join, in both estimators, held by a corpus case.

## Plan

Found by the [target fall](https://github.com/moq-dev/moq/pull/8677)
prototype on the 11-11 to 11-13 Nightly captures: the relay delivers the
opening backlog newest first, interleaved with the live edge, and the
estimator measures each backlog frame against the older backlog frames
before it, at 250 to 330 ms. The first one or two intervals fold that at
the cold start's `FRESH_MAX`, 10% each, so the target sits at 320 to 360 ms
for about 20 s on every congestion row, and its residue near the 5% line
lets the next late interval tip it back up. The player never waits for
those frames: a join skips the backlog onto the clock.

Candidates, to judge by the Nightly rows as the target fall was:

- Measure a reordered frame against the whole window's fastest frame when
  that frame is far ahead of the frame's own reference, so backlog behind
  the clock reads as skipped rather than late.
- Do not let an observation above the current target take `FRESH_MAX`; the
  prototype found this costs `wide` underruns during the cold start.
- Start measuring at the first frame the player plays.

Recommended: the first, which keeps the cold start's quick convergence.
