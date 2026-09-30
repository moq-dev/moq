# [M] Edge and core relays

## Goal

A relay runs as an edge or a core, and a cluster built from them carries
each broadcast into a region once. An edge serves end users, dials every core
in its region, spreads paths over those cores by rendezvous hashing, and is
never transit between cores. A core serves only cluster peers, dials the
cores of the regions it links to, and never dials an edge.

## Plan

Decided in the 2026-09-30 wildcard audit (cache tiers):

- The role is the node's own `cluster.role`, explicit, from config or
  alongside the peer list the connect API returns. A relay whose links
  contradict its role (an edge linked to another edge, a core dialing an
  edge) fails at startup. Where a region has one edge, that edge is its core.
- A core refuses a session that is not a cluster peer, loudly, so a
  misrouted client fails instead of silently exposing the shield.
- Cores dial only cores with lower names, so each pair has one connection;
  cores in one region do not link to each other. This is configuration: the
  relay follows its peer list, and whatever generates the list applies the
  rule.
- Edge links run qmux over TLS on TCP, not WebSocket and not QUIC:
  intra-region links are not congested. qmux over TCP exists today only in
  plaintext (`tcp://`); add `tls://`, wired to qmux's existing `tls` module.
  (Scheme name decided 2026-09-30.)
- An edge never re-advertises a route learned from one core to another core:
  its cluster dials publish only what entered locally
  (`origin::Consumer::local()`), so no core routes through an edge.
- An edge picks a core per path with the path-keyed rendezvous hash the
  wildcard line gave `route_order`, so every edge in a region sends a path to
  the same core, and failover moves only the paths the lost core served. The
  per-path spread across equally ranked cores is
  [Selection](/quest/m1/cluster-routing/selection.md)'s.

Test: a two-region cluster (two edges and two cores in one region, one core
in the other) in `rs/moq-relay/tests`, where subscribers on both edges of a
region cause one cross-region subscription per broadcast, a route from one
core never reaches another through an edge, a client dialing a core is
refused, and losing a core moves only its paths.

Public API: a relay config field for the role and a TLS qmux URL scheme.
Wire: none expected.

## Related

- [moq-transport peers are plain clients](/quest/m1/ietf-cluster-off.md) - cluster links are lite-only
