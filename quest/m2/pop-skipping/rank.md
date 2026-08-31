# [L] Rank

## Goal

Rank warm relay candidates by cold cost before hop count. Carry cold cost
beside marginal cost on Lite06 WIP, prefer the lower cold cost ahead of hop
count where marginal cost ties, make adoption descend `(cold cost,
per-broadcast relay hash)`, and hold a re-parent onto another relay long
enough for the costs it rests on to land, with asymmetric, equal-rank,
transitive, simultaneous-handover, stale-ring, and group-boundary tests.
