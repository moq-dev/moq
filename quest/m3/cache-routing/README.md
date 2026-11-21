# Cache-aware routing

## Goal

A relay pulls each broadcast from the cheapest copy: a neighbour that
already has demand for its Instance when that costs less than a fresh copy
from the source. A join stops at no pop without demand, drones with their
own uplinks pull one copy over one uplink, edges that prefer different cores
share one copy between regions, and no link carries a group twice. Routing
converges once demand and topology stop changing, and a broadcast nobody
reads costs no message of its own.

Non-goals: steering clients to the relay that holds a broadcast; decisions
from measured load or RTT; browsers serving copies they receive; and creating
links, since routing chooses only among the sessions an operator configured.

## Plan

Reopened 2026-11-15 (maintainer, /quest-plan on
[cache-aware multicast](https://github.com/moq-dev/moq/blob/41cc107579/quest/m2/multicast/README.md), requirement 8:
"there is a cold cost for a route and a warm cost", and routes coalesce onto
a node that says it carries a broadcast). What stands and what goes:

- ✅ Joins stop at the first relay carrying the Instance, which the route
  layer gives with no message
  ([requests](https://github.com/moq-dev/moq/pull/8881)).
- ✅ How a relay learns of a warm neighbour off its path is picked from
  measurements ([warmth](https://github.com/moq-dev/moq/pull/8903)), which
  [the implementation](/quest/m3/cache-routing/select.md) then builds.
- ✅ A warm cost is strictly positive and summed from the source along the
  path the data takes. This replaces item 14's rule below, under which a
  carrier offered its copy at no cost and a hash ranked carriers.
- ✅ Warm means demand, not cache contents, and pop skipping and one uplink
  per group of drones are the two cases that must work (maintainer,
  2026-11-15, [epic](https://github.com/moq-dev/moq/blob/41cc107579/quest/m2/multicast/README.md) item 9).
- ✅ A pull moves to a cheaper warm copy only from a reader ranked below its
  own: ranked readers, which a TLA+ model shows settle (maintainer,
  2026-11-17, [moq-dev/moq#8986](https://github.com/moq-dev/moq/pull/8986);
  rejected: settled pulls). Its wire joins lite-07 only if the port is
  ready when the cut's other blockers clear (maintainer, 2026-11-19);
  otherwise it lands in the first wip version after the cut. The port is
  measured and not yet built, pending the maintainer (the coordinating
  session, under the maintainer's delegation, 2026-11-19).
- The non-goal of "one copy across a multi-hop sparse overlay rather than
  among direct neighbors" is dropped: a mesh is the product this serves.

The earlier Plan, items 1 to 14 (2026-11-08 to 2026-11-14), is in
`git show bf04cecfe1:quest/m3/cache-routing/README.md`. What it measured:

- Cache-aware cost was built four times before it (2026-07 to 2026-08), and
  each time two warm relays adopted each other.
- Item 4's rule rested on Babel's feasibility distance, which cannot let a
  relay join a copy at a neighbour no closer to the publisher.
- Item 14's rule settled every one of 72,000 sweep scenarios and sent about
  3.1 warm messages per reader change, but its pulls formed a cycle in 3.3%
  of churn steps, about 9 in 10 of which left a reader unfed, and its gain
  over plain path routing was never measured
  ([moq-dev/moq#8732](https://github.com/moq-dev/moq/pull/8732)). The
  maintainer stripped its wire from lite-07 on 2026-11-14
  ([moq-dev/moq#8840](https://github.com/moq-dev/moq/pull/8840)).

This README's own work, once its child lands:

- End-to-end tests over real sessions, time mocked. A chain of three relays
  plus a skip link: a cold broadcast takes the skip, a reader on the skipped
  relay makes the far relay join it, and that reader leaving sends it back.
  Two LAN relays with their own uplinks and readers on both pull one copy
  over one uplink when the readers arrive in turn, and when they arrive at
  once, the higher-ranked relay moving onto the other's copy. The other
  uplink takes over when that one is lost.
- Docs: warm and cold costs in `doc/bin/relay/cluster.md`, and a section on
  the routing concept page once the
  [routing guide](/quest/m3/routing-guide.md) has added it.

## Related

- [Cluster routing](/quest/m3/cluster-routing/README.md) - the node routes this ranks on
