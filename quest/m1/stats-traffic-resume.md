# [S] Stats traffic resume

## Goal

The stats aggregator's merged per-path traffic tracks (`[<tier>/]publisher.json`
and `subscriber.json` through `aggregate::Consumer::traffic`) never lower a
counter when a node resumes in the same epoch after a departure. Today a
departure closes the node's open pairs (announces, broadcasts, subscriptions),
so the merged `*_ended` counters rise to `*_started`; when the same epoch's
reader comes back with those pairs still open, its raw readout replaces the
closed one and the merged ends drop back down, which a cumulative reader sees
as a negative delta or a false reset.

## Plan

Decided 2026-10-13 (follow-up from #5084):

- Use the fix #5084 landed for the totals and presence tracks: when a stale
  node resumes without regressing, add its closed pairs back as that many
  starts and ends (`Mergeable::reopen` in `rs/moq-stats/src/aggregate.rs`),
  so the live count stays exact and no counter goes down.
- Per-path traffic is `RESETS = false` today because a per-path map drops
  keys and restarts counters as entries end, and that single-node reset
  contract stays: a counter going down on a route without an epoch still
  reads as a reset. Only the resume of a stale node changes, so decide
  whether reopening can apply to traffic without also adopting the totals'
  baseline-on-reset behavior.
- Test a same-epoch depart and resume under one held path, checking every
  merged frame, beside the existing `a_resume_reopens_closed_pairs_without_going_down`.
- Update the `aggregate::Consumer` doc comment, which describes the per-path
  reset contract.

Public API: none. Wire: none; only the merged values an aggregator emits.

## Related

- [Stats prefix tracks](/quest/m1/stats-prefix-tracks.md) - its
  merged prefix tracks need the same resume rule
- [QoS](/quest/m1/qos/README.md) - histograms that land on the same tracks
