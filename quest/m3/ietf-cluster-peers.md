# [L] moq-transport cluster peers

## Goal

A moq-transport relay joins a cluster as a peer through an extended
`draft-lcurley-moq-cluster`, with the loop safety a lite cluster link has.

## Plan

The extension must carry what a lite cluster link carries by then: the route
layer of the [cluster routing line](/quest/m1/cluster-routing/README.md)
(per-node ROUTEs with seqno and metric, path-less announces, the down-only
bit) and its selection.

Test: a moq-transport relay peering through the extension discards a route
that loops back to it, and resumes a subscription on another route when the
serving one dies, only with matching explicit epochs. Test cluster-only,
epoch-only, both and neither negotiated: cluster metadata alone never makes
namespace equality sufficient for continuity. Without verified epochs,
route replacement signals a new instance instead of stitching.

Noted 2026-10-31 from [moq-dev/moq#7259](https://github.com/moq-dev/moq/pull/7259):
the draft's "Track Delivery" section now runs ahead of the implementation.
It gives PUBLISH and SUBSCRIBE_TRACKS the namespace advertisements' Hop ID
loop rules: a PUBLISH carries HOP_PATH and ROUTE_COST, is sourced only from a
path that excludes the subscriber's Hop ID, and is discarded when its path
holds the receiver's own Hop ID; an edge relay forwarding an unsolicited
PUBLISH keeps its HOP_PATH (or the bridging 0 entry), appends its Hop ID, and
accumulates ROUTE_COST; and SUBSCRIBE_TRACKS resolves per namespace and
resumes a track's preferred source only while the track has demand. Neither
`rs/moq-net` nor `js/net` carries HOP_PATH or ROUTE_COST on PUBLISH yet.
Implement both here, and reconcile with
[moq-transport refuses SUBSCRIBE_TRACKS](/quest/m1/ietf-tracks-refuse.md),
whose publishers refuse inbound SUBSCRIBE_TRACKS: once this lands, a
cluster peer serves it instead.

Wire: extends `draft-lcurley-moq-cluster`, updated in the same PR.

## Required

- [Cluster routing](/quest/m1/cluster-routing/README.md) - settles what a cluster link carries
