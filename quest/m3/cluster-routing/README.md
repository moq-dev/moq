# Cluster routing

## Goal

One routing model serves CDN tiers, outbound-connected local clusters, and
application-selected peer connections. Topology changes update reachability
per affected routing node rather than replaying every advertisement behind it.
Known advertisements remain distinct from current reachability, and stale
copies cannot revive a claim after its withdrawal is known.

Reliability and federation are independently shippable milestones of this
architecture. Peer-to-peer routing left the repository 2026-11-14
(maintainer); this model stays what it builds on. Active/passive delivery health is out of scope; its
plan was deleted 2026-11-09 for want of a consumer.

## Plan

Moved from m1 to m3 2026-11-13 (maintainer, /quest-plan): it waits on m2
work, and no quest requires one in a later milestone, since a blocker ranks
by its own milestone. ✅ m3 (rejected: m2, where the cut would wait on it;
pulling the m2 blocker into m1). It ranks right after m2, when it can start.

Reopened 2026-11-15 (maintainer, /quest-plan on the whole route layer):
[cache-aware multicast](https://github.com/moq-dev/moq/blob/41cc107579/quest/m2/multicast/README.md) replaced the route
layer this Plan described, and where the two differ it wins. A routing node
is a node that forwards, not every publisher; a claim crosses a scoped grant
by a chain, not a qualified identity under a route key; and holds, doubt,
and the non-goal of cache-dependent route prices are gone. One model for CDN
tiers, outbound local clusters, and meshes stands.

What stands of the 2026-10-13 decisions (maintainer), restated for the route
layer [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) shipped.
The rest of the earlier Plan (Babel's feasibility, every publisher an origin
node with a declared identity, qualified identities under a route key, holds,
and doubt) went with the old router; it is in
`git show 4e73451999:quest/m3/cluster-routing/README.md`.

- Routes and claims have separate lifetimes on the ordered Announce Stream.
  A wildcard claim is a capability, not a list of live broadcasts;
  withdrawing one claim does not end other claims of its Instance.
- Applications choose connections and assign each a local priority, whether
  it reaches a CDN or a peer. A route ranks by drain, then the lowest
  priority on its path, then additive cost. Link costs are strictly positive
  (default one per link): zero is refused, and there is no hop-count
  component. Equal lite-07 claims spread by the draft's Rendezvous Hash
  (a claim from an older version compares its hop chain's length first),
  and among otherwise identical routes a node stays on its next hop.
- The first hop selects the claim's routing node; forwarded requests name it
  and aggregation preserves it.
- GOAWAY makes every route through the node draining, ranked below every
  route that avoids it and kept as a last resort while connected. Drain is a
  rank, never a saturated metric. Instant failover is not required: measure
  failure detection, routing recovery, and total interruption separately.
- A subscription moves only between claims of one Instance; another Instance
  is a restart, and neither a node's identity nor a healthy alternate grants
  permission to splice content.
- While no route reaches a claim it stays shown, unavailable, until the
  reader's debounce lets it go; route loss never manufactures an END, and
  no stale state is kept unbounded.
- The `upstream` link mark, now a route's mark, keeps CDN relays from
  shortcutting through a local network while CDN content still flows in and
  local content out, including through a peer whose own uplink died. A
  strictly cheaper marked route still wins; rank local peers above a backup
  uplink by priority to keep one copy per broadcast.
- A local per-connection `proxy` preference decides whether a connection is
  sent routes learned from other connections. Relays default on;
  applications choose per peer connection (on for redundancy, off for
  conference offload). Browser tabs stay off until the
  [Rust model runs in the browser](/quest/m5/rs2ts-js.md). Applications
  explicitly select peer connections; discovery does not dial everyone.
- Each CDN broadcast enters a local network once by configuration: local
  peers outrank a backup uplink, and origin aggregation joins the rest.
- A peer-to-peer consumer, outside this repository, owns proving that its CDN
  session and discovery stay connected and unused CDN media demand is released
  after a handoff.

Existing mesh deployments remain supported; the full edge/core topology
migration is independent. Basic outbound federation proves the common routing
contract without requiring that deployment migration.

Recent baseline: [moq-dev/moq#5375](https://github.com/moq-dev/moq/pull/5375)'s
two-second retraction quarantine reduced stale revivals but left residual
cases; [moq-dev/moq#5391](https://github.com/moq-dev/moq/pull/5391) bounded
its queue under repeated same-clock churn.
[moq-dev/moq#5382](https://github.com/moq-dev/moq/pull/5382) and
[moq-dev/moq#5393](https://github.com/moq-dev/moq/pull/5393) separately
reduced idle-session polling. Keep regression coverage for announcement
churn, idle work, active continuity, and retained state separately; fewer
control bytes does not prove cheaper runtime work.

Non-goals: measured RTT/loss pricing by default, TURN, browser transit, full
link-state topology, or cross-operator economic exchange rates. Basic
federation does not wait for
administrative routing cost domains, which nobody plans. Node routes here
are the cold cost that [cache-aware routing](/quest/m3/cache-routing/README.md)'s
per-broadcast warm routes rank against (decided 2026-11-08, maintainer).

Each implementation updates the docs it makes stale. The separate
[routing guide](/quest/m3/routing-guide.md) explains the combined design and
setup after the capabilities land.

## Related

- [Routing guide](/quest/m3/routing-guide.md) - concept page and federation setup
- [Route wakes](/quest/m3/route-wakes.md) - local work follows affected fronts rather than the whole covered table
- [Cache-aware routing](/quest/m3/cache-routing/README.md) - per-broadcast warm routes ranked against these node routes
