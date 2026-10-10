# [XS] Drop the hidden cluster exemption

## Goal

The relay stops treating authenticated cluster peers as opted in to hidden
broadcasts. Every relay opts in on the wire (moq-lite-07's `Hidden` field or
the MoQ Hidden `SUBSCRIBE_NAMESPACE` parameter), so the exemption only exists
for peers that predate it.

## Plan

In m3 because it waits on a deployment, not on code; it comes back once
the mesh runs lite-07.

- Remove the `cluster_peer` exemption, now a local inside `Cluster::scope`
  (`rs/moq-relay/src/cluster.rs`, with its TODO), so an inbound peer discovers
  hidden routes only when it asks, and the forced `with_hidden(true)` on the
  outbound dial in the same file. `connection::authorize` no longer exists on
  `main`; only `release` still has that shape.
- Replace `rs/moq-relay/tests/hidden_cluster.rs` with a lite-07 mesh test that
  still sees a `.`-prefixed hidden broadcast.

## Required

- [The moq.pro mesh runs lite-07](/quest/m3/lite07-mesh.md) - every peer opts in on the wire
