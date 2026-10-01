# [S] moq-transport peers are plain clients

## Goal

No moq-transport session negotiates the MoQ Cluster extension
(`draft-lcurley-moq-cluster`). A moq-transport peer is always served as a
plain client: it never receives Hop IDs or relay costs and never acts as a
cluster link. Cluster links are moq-lite only. moq-transport end users stay
fully supported.

## Plan

Why now: the Wildcard line spreads a pool's paths behind one route, and names
the serving member only in lite-07's reply Origin. The cluster extension has
no such field, so a moq-transport downstream relay pins failover to the
advertisement's first Hop ID, which labels the pool rather than the member,
and can splice one member's objects onto another's. A peer that never learns
Hop IDs cannot make that mistake. Decided in the 2026-09-30 wildcard audit,
matching the topology decision that cluster links are lite-only.

- Stop offering and accepting the cluster Setup Options in Rust
  (`rs/moq-net/src/ietf/cluster.rs`, `parameters.rs`, `publish_namespace.rs`)
  and JS (`js/net/src/ietf/cluster.ts` and its callers). A peer that offers
  them is served as if it had not.
- Delete what becomes dead, in both languages. The draft stays:
  [moq-transport cluster peers](/quest/m2/ietf-cluster-peers.md) extends it
  and brings the code back.
- Update `doc/bin/relay/cluster.md` and `doc/concept` wherever they say
  moq-transport relays can join a cluster.
- Test: a moq-transport 17+ peer that offers the extension gets a session
  with no cluster parameters, and an announcement reaches it with no Hop IDs.

Lands on `main`: extension negotiation is optional, so a peer that offered it
still gets a working session. Public API: none expected. Wire: we stop
negotiating an optional extension.

## Related

- [Non-transit relays](/quest/m1/cluster-routing/transit.md) - cluster links are lite-only
- [Wildcard](/quest/m0/wildcard/README.md) - the pool spread that makes a Hop ID label unsafe to splice on
