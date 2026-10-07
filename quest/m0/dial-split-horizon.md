# [S] Dialed sessions get split horizon

## Goal

A session moq-net dials, whose peer negotiates no identity, never has a
route it learned from that peer offered back to it, and a SUBSCRIBE it sends
never routes back to it. Today such a session is anonymous: moq-relay
dialing moxygen, moqx or moq-rs over moq-transport (d16, or d17+ without
HOP_ID) echoes the peer's namespaces back, and both sides subscribe to each
other while no objects flow.

## Plan

Triaged from Fastly's moq-relay-interop report (2026-09-23 run; items 3 and
13's echo) on 2026-10-07. The draft leaves loop prevention to
implementations (d16 §6.2).

Facts from `main`:

- Split horizon keys on the peer's identity: `exclude()` is
  `peer.identity().or(self.peer_hop).unwrap_or(UNKNOWN)`
  (`rs/moq-net/src/ietf/publisher.rs`, around line 445), and `Hop::UNKNOWN`
  excludes nothing (`visible_to`, `rs/moq-net/src/model/origin.rs`).
- Accepted sessions already get a random assigned hop
  (`rs/moq-net/src/server.rs`, around lines 231, 381 and 417).
- `Client::with_peer_hop` (`rs/moq-net/src/client.rs:128`) exists, but the
  relay's cluster dial (`run_remote_session`, `rs/moq-relay/src/cluster.rs`)
  never calls it. An `upstream: true` link already hides upstream-learned
  routes, but that is not the default.

Decided 2026-10-07: fix it in moq-net's `Client`, not only the relay's
dial. A session with no negotiated identity gets a random per-connection hop,
the same way `server.rs` does. Rejected: calling `with_peer_hop` from
moq-relay only, which leaves other moq-net users anonymous. Check whether
`with_peer_hop` still has a caller afterwards, and delete it if not.

The same gap let a d16 moq-dev origin route its edges' SUBSCRIBEs to two of
the edges (the report's T4 fanout-late-join, two runs). Check that this case
is covered too.

Test: a dialed anonymous ietf session that announces a namespace does not
receive it back, and a SUBSCRIBE that arrives on it is not routed back to it.

Public API: possibly removes `Client::with_peer_hop`. Wire: none.

## Related

- [moq-transport cluster peers](/quest/m2/ietf-cluster-peers.md) - multi-hop loop safety for moq-transport peers through the cluster extension
- [Cluster routing](/quest/m1/cluster-routing/README.md) - the route layer split horizon feeds
