# [M] The target falls back soon after a one-off spike

## Goal

After a one-off arrival spike, such as the audio-quality `congestion` row's
1.5 s outage, the playout target, and with it video's delay, returns to its
level before the spike sooner than today's 10 to 23 s, without more underruns
on the `wide`, `bursty`, and `step` rows. One rule in
`doc/concept/audio-jitter.md`, held by its corpus, in both estimators.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan): ✅ the implementer prototypes
and picks by the Nightly rows (rejected: a shorter half-life overall now;
counting an outage as one observation now). This reopens the 2026-11-09
decision that the estimator stays as it is; the
[epic](/quest/m2/audio-playout/README.md) records the rest.

Why it holds: each 500 ms interval of the outage folds its own peak, so three
or four folds put about 7% of the weight past the 5% the 0.95 quantile
needs, and the 20 s half-life takes 10 to 23 s to decay it. The 11-13 Opus
row's target fell 1020, 720, 340, then 140 ms, holding the whole broadcast
seconds behind live after the link had recovered.

Candidates:

- Count one outage, the empty intervals and the burst that ends them, as one
  observation.
- Decay the weight above the current target faster than the rest.
- A shorter half-life overall, which re-stalls more on periodic bursts.

Judge each by how long the congestion row's target stays raised and by
underruns on the other rows, in both players, and add corpus cases for the
pick. A fall is lag like any other under the
[lag rule](https://github.com/moq-dev/moq/pull/8722), so a fall past the
threshold costs one snap rather than a slow catch-up.
