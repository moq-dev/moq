# [M] Cluster simulator

## Goal

A deterministic, seeded simulator exercises real moq-net origins and route
selection under controlled event delivery. It establishes a flooding baseline
and supports pruning prototypes before a cluster wire format exists. Failing
seeds and delivery schedules replay exactly through just check.

## Plan

Use in-process transports and paused runtime time. Start with today's sessions
and flooding; inject prototype control events independently of announcements,
then reuse the harness for the actual cluster stream. Do not make this quest
depend on that stream. A model can explore an algorithm but does not replace
coverage of real cursors, source pins, and subscriptions.

Generate connected meshes with weighted and zero-cost links, multiple sources
for one prefix, warm exact-path routes beside broader claims, anonymous hops,
parallel sessions, mixed versions, drains, and cost saturation. Schedule starts,
withdrawals, repricing, link cuts/restores, relay restarts, and mode changes.
Reorder delivery across sessions and streams while preserving each stream's
ordering; no wall-clock sleeps.

Compare candidate runs with flooding on the same topology and source events:

- After convergence, reachable prefixes and selected source identities and
  ranks agree. Holding some route is insufficient if pruning lost the better
  source or the identity pinned by an existing subscriber.
- Healthy handoffs do not lose a usable route because an old advertisement
  was pruned before its replacement arrived. Check transitions, not only the
  final state.
- For failures, record retained usable standbys and recovery events separately
  from media resubscription and playback. Apply the policy's explicit recovery
  contract; do not infer continuity from an announcement alone.
- Chains remain loop-free. Count copies per prefix and per source separately,
  with temporary overlap and flooding cases reported rather than rejected by
  an unconditional two-copy assertion.

Named cases must include a chosen backup preferring another source and sending
nothing after filtering; an equal-cost reachability path avoiding a node while
the broadcast path uses it; three sources of one prefix; a disjoint old/new
parent set with delayed replacement delivery; an empty local chain; an unknown
hop before a known relay; and new components connected through old or IETF peers.

Report starts, ends, updates, bytes, and control-state churn for initial discovery,
steady changes, and recovery separately. Bound the seed count in just check and
provide a recipe for longer sweeps. Later implementation quests extend this
harness with their wire and regression cases.

## Required

- [Announce counters](/quest/m1/announce-tree/counters.md) - comparable measurements for candidate and flooding runs
