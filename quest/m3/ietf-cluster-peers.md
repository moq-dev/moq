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

Wire: extends `draft-lcurley-moq-cluster`, updated in the same PR.

## Required

- [Cluster routing](/quest/m1/cluster-routing/README.md) - settles what a cluster link carries
- [IETF epochs](/quest/m1/ietf-epochs.md) - explicit identity for the seamless-resume guarantee
