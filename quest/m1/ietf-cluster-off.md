# [S] moq-transport peers are plain clients

## Goal

No moq-transport session negotiates the MoQ Cluster extension
(`draft-lcurley-moq-cluster`). A moq-transport peer is always served as a
plain client: it never receives Hop IDs or relay costs and never acts as a
cluster link. Cluster links are moq-lite only. moq-transport end users stay
fully supported.

## Plan

Why: cluster links are moq-lite only (the 2026-09-30 topology decision), so
a moq-transport peer stays a plain client until
[moq-transport cluster peers](/quest/m2/ietf-cluster-peers.md) extends the
draft.

- Stop offering and accepting the cluster Setup Options in Rust
  (`rs/moq-net/src/ietf/cluster.rs`, `parameters.rs`, `publish_namespace.rs`)
  and JS (`js/net/src/ietf/cluster.ts` and its callers). A peer that offers
  them is served as if it had not.
- Delete what becomes dead, in both languages. The draft stays, for
  moq-transport cluster peers to extend.
- Update `doc/bin/relay/cluster.md` and `doc/concept` wherever they say
  moq-transport relays can join a cluster.
- Test: a moq-transport 17+ peer that offers the extension gets a session
  with no cluster parameters, and an announcement reaches it with no Hop IDs.

Lands on `main`: extension negotiation is optional, so a peer that offered it
still gets a working session. Public API: none expected. Wire: we stop
negotiating an optional extension.

## Related

- [Upstream links](/quest/m1/cluster-routing/transit.md) - cluster links are lite-only
