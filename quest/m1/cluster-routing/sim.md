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

moq-dev/moq.pro#2136 rewrites that quest for the split; until it merges the
moq.pro quest still asks the older path-vector question. To check: look for
the report in a merged moq.pro PR. To advance: land #2136, then ask the
maintainer to start the moq.pro quest. Delete this quest once the report is
in and [Routes and announces](/quest/m1/cluster-routing/routes.md) is
confirmed or revised by it.
