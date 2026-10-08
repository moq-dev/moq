# [S] Direct peers win

## Goal

A subscriber uses a direct P2P link only when the peer at the other end is
the broadcast's origin or a routing node (a `moq-cli --p2p` hop or a relay
the customer hosts), and the CDN otherwise. A browser never carries another
peer's traffic. The rule is the route layer's ordinary metric over link
costs the node sets itself, with defaults that make a direct link beat the
relay, so a watcher next to the publishing tab pulls from it and everyone
else pulls from the CDN.

## Plan

Decided 2026-10-01 (moq-dev/moq#4694):

- No warm or cold cost and no cache state. A peer that merely watches a
  broadcast does not offer it to other peers; deduplicating a site's internet
  traffic is a customer relay's job, which is a routing node like any other.
  This deleted the JS transit quest.
- Link costs are the node's own policy: `Peers` sets the cost of P2P links
  and of the relay session, mirrored by `moq-cli --p2p`. The relay declares
  nothing per client. Pick defaults so one direct LAN hop beats the relay
  route, and a relay route beats two P2P hops through a routing node only
  when the application says so.
- The comparison needs the node ids and metrics of
  [Routes and announces](/quest/m1/cluster-routing/routes.md): a broadcast's
  ANNOUNCE names its origin node, and the roster maps that node to a peer the
  application may dial, so an app learns whom to dial from the announce.
- Switching between the peer and the relay resumes the subscription, since
  every route announcing a path under one
  [publisher epoch](/doc/concept/moq-lite.md#publisher-epochs) is one source.

Deliverables: the `Peers` and `moq-cli` cost knobs with their defaults, the
rule written beside route selection in the routing concept page the cluster
routing line adds, and tests with the mock transport: a watcher with a direct
link to the publishing tab uses it and returns to the relay when it drops; a
watcher whose only direct peer is another watcher stays on the relay.

Public API: cost knobs on `Peers` and `moq-cli`. Wire: none beyond the route
layer.

## Required

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the origin node ids and metrics this compares
