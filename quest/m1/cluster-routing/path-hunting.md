# [S] Tiered path hunting

## Goal

Path vector on the edge and core layout never revives an ended path and
settles quickly when an origin ends or a core is lost. If a measurement shows
it hunts stale alternatives, it is fixed at the cause.

## Plan

Replaces the propagation plan, which assumed a link-state topology and an
existence split. On the flat mesh the simulator saw an origin's end explore
stale alternatives for about a second and tens of thousands of announces,
and #4399's withdrawal hiding traded that for flapping, driven partly by
Warm repricing. Tiers confine hunting to a sparse core graph and remove
Warm, so the remaining risk may be small.

- If stale alternatives remain, carry a per-origin seqno, scoped to the
  origin's incarnation, with each announcement beside its hop list, and never
  apply an event older than one already seen for that path and origin
  (DSDV and Babel feasibility). Keep an ended path's seqno long enough to
  outlive delayed copies of its start.
- If nothing remains, delete this quest with the measurement as the reason.

Wire, if seqnos are needed: a field in the current wip lite version, with
the draft updated in the same PR.

## Required

- A simulator run of the tiered layout ([moq.pro's routing simulator](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/routing-sim-tiers.md)) that records announces and convergence when an origin ends and when a core is lost

## Related

- [Cross-relay delivery under bursts](/quest/m1/cross-relay-bursts.md) - closed broadcasts announced for minutes, evidence for per-origin seqnos
