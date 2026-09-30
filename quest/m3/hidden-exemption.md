# [XS] Drop the hidden cluster exemption

## Goal

The relay stops treating authenticated cluster peers as opted in to hidden
broadcasts. Every relay opts in on the wire (moq-lite-07's `Hidden` field or
the MoQ Hidden `SUBSCRIBE_NAMESPACE` parameter), so the exemption only exists
for peers that predate it.

## Plan

In m3 because it waits on a deployment, not on code; it comes back once
the mesh runs lite-07.

- Remove the `cluster_peer` argument from `connection::authorize` and its
  callers in `rs/moq-relay/src/{connection,uring,websocket}.rs`, and the forced
  `with_hidden(true)` on the outbound dial in `rs/moq-relay/src/cluster.rs`.
- Replace `rs/moq-relay/tests/hidden_cluster.rs` with a lite-07 mesh test that
  still sees a `.`-prefixed hidden broadcast.

## Required

- The moq.pro mesh deploys lite-07
