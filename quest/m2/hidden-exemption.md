# [XS] Drop the hidden cluster exemption

## Goal

The relay stops treating authenticated cluster peers as opted in to hidden
broadcasts. Every relay opts in on the wire (moq-lite-07's `Hidden` field or
the MoQ Hidden `SUBSCRIBE_NAMESPACE` parameter), so the exemption only exists
for peers that predate it.

## Plan

- Remove the `cluster_peer` argument from `connection::authorize` and its
  callers in `rs/moq-relay/src/{connection,uring,websocket}.rs`, and the forced
  `with_hidden(true)` on the outbound dial in `rs/moq-relay/src/cluster.rs`.
- Replace `rs/moq-relay/tests/hidden_cluster.rs` with a lite-07 mesh test that
  still sees `.internal/origins`.

## Required

- Every deployed relay in the moq.pro mesh speaks a finalized moq-lite-07 or MoQ Hidden; `moq-lite-07-wip` is opt-in only.
