# [S] Cluster burst drill always overruns its bottleneck

## Goal

`bursts_cross_a_cluster::impaired` (`rs/moq-relay/tests/drills.rs`) always
exercises its bottleneck: every run has at least one burst that overruns
the 100 kbit/s link, so its "no burst overran a bottleneck, so none was
exercised" precondition never fails.

## Plan

Found in #5160's loaded runs (2026-10-09): 5 of about 450 runs of the drills
binary at load average 40-60 failed that precondition. Decided 2026-10-09:
keep the precondition, since a run that never overruns tests nothing. Find
why a burst sometimes fits (for example, pacing that lets the shaper drain
between frames under load), then size or schedule the bursts from the
shaper's capacity so the overrun is guaranteed. No retry, and no lowering
of what counts as an overrun.

Public API: none. Wire: none.
