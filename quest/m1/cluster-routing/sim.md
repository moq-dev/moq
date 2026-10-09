# [XS] Simulate the split

## Goal

Waits on moq.pro's routing simulator. The condition clears when moq.pro's
[Simulate the edge and core layout](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/routing-sim-tiers.md)
reports a ROUTE and ANNOUNCE split candidate (per-node distance vector with
Babel feasibility, path-less announces following the next hop) beside path
vector, on the tiered live layout, the tiered synthetic sweeps, and a
synthetic drone mesh (lossy links, two uplinks, a partition that heals).

Per scenario (publish, end, link flap, relay loss and restart, uplink loss):
messages and bytes by kind, convergence time, stale-path seconds, transient
subscribe loops, and whether a two-uplink mesh ever carries CDN traffic.

moq-dev/moq.pro#2136 rewrote that quest for the split (merged 2026-10-02),
and moq-dev/moq.pro#2152 (draft) measures the tiered route and mesh failures.
Its report says the split needs a design revision before its wire ships, so
the condition is now the maintainer's decision on that revision, not only a
merge. To check: #2152 merges, or the maintainer decides the revision. To
advance: ask the maintainer to review #2152's findings. Delete this quest
once [Routes and announces](/quest/m1/cluster-routing/routes.md) is
confirmed or revised by them.

## Plan

Findings from #2152 (as of 2026-10-03) for
[Routes and announces](/quest/m1/cluster-routing/routes.md), which should
carry them now rather than wait for the merge:

- A five-node ring keeps a working longer backup yet stays starved for good
  until the origin advances its sequence. The report recommends Babel
  sequence-number requests.
- On origin-end over the live tiers, the split sends far fewer bytes than
  path vector but still about 2150 client updates against an 840 minimum
  (up to 9 per relay and path) and 655 re-announces, against that quest's
  goal that ending a broadcast reaches each node about once.
