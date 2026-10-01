# [M] Per-origin sequence numbers end path hunting

## Goal

When a broadcast ends, each relay retracts it once and never re-announces a
stale alternative, and a failover where the origin still publishes stays
seamless: no downstream front loses its route while a working one exists.

## Plan

History (2026-09-30 to 10-01): moq-dev/moq.pro#2116 measured ended names
drawing 575 to 57,805 mesh announce starts on Kick dev, and reproduced it on
live's 34-relay graph. #4642's 300 ms announce-cursor hold mitigates it and
is what production runs; it still leaves 16 of 34 publishers hunting in the
repro. #4644 added a cluster-link hold-down (retract at once, re-announce a
replacement after 1 s) and measured 0 resurrections, but the maintainer
rejected it: the immediate retraction ends a downstream peer's front on
`best: None` when its only path ran through the relay, so subscribers drop
for about 1.1 s while the relay keeps serving. Holding the replacement
without retracting brought hunting back. A lite-06 announce-side fix cannot
avoid that retraction.

Decided: carry a per-origin seqno, scoped to the origin's incarnation, with
each announcement on cluster links in the wip lite version, and never apply
an event older than one already seen for that path and origin (DSDV and
Babel feasibility). A withdrawal then outranks every stale copy without a
hold-down or a retraction of live alternatives.

- Start from #4644's branch (`quest/m0/path-hunting`): its live-graph
  regression tests (withdrawal at several latencies, and failover) fail on
  `main` and stay the acceptance tests. Add a check that a downstream
  in-flight subscription survives the failover.
- Keep an ended path's seqno long enough to outlive delayed copies of its
  start; a new incarnation clears it.
- Decide whether #4642's cursor hold can then be removed.
- Clients and lite-06 cluster peers are unchanged; only wip-version cluster
  links carry the field.

Wire: a field on ANNOUNCE_START and ANNOUNCE_END in the wip lite version,
with `drafts/draft-lcurley-moq-lite.md` updated in the same PR.

## Related

- [Edge and core](/quest/m1/cluster-routing/roles.md) - tiers shrink the core graph this hunts on
- [Cross-relay delivery under bursts](/quest/m1/cross-relay-bursts.md) - closed broadcasts announced for minutes, the same symptom
