# [M] Edge and core relays

## Goal

A relay runs as an edge or a core, and a cluster built from them carries
each broadcast into a region once. An edge serves end users, dials every core
in its region, spreads paths over those cores by rendezvous hashing, and is
never transit between cores. A core is hidden from end users, dials the cores
of the regions it links to, and never dials an edge.

## Plan

Decided in the 2026-09-30 wildcard audit (cache tiers):

- The role is explicit and comes from where the peer list comes from:
  config, or the connect API alongside the peers it returns. A relay whose
  links contradict its role (an edge linked to another edge, a core dialing an
  edge) fails at startup. Where a region has one edge, that edge is its core.
- Cores dial only cores with lower names, so each pair has one connection;
  cores in one region do not link to each other.
- Edge links run over TLS (qmux), not QUIC: intra-region links are not
  congested. Check what the relay's cluster dial supports today.
- An edge never re-advertises a route learned from one core to another core,
  so no core routes through an edge.
- An edge picks a core per path with the same path-keyed rendezvous hash the
  wildcard line gave `route_order`, so every edge in a region sends a path
  to the same core. Failover moves only the paths the lost core served.

Test: a two-region cluster (two edges and two cores in one region, one core
in the other) in `rs/moq-relay/tests`, where N subscribers on both edges of a
region cause one cross-region subscription per broadcast, and losing a core
moves only its paths.

Public API: a relay config field for the role. Wire: none expected.

## Related

- [Topology](/quest/m1/cluster-routing/topology.md) - under review; may shrink to cores only
- [moq-transport peers are plain clients](/quest/m1/ietf-cluster-off.md) - cluster links are lite-only
