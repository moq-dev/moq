# [L] moq-transport cluster peers

## Goal

A moq-transport relay can join a cluster as a peer again, through an
extended `draft-lcurley-moq-cluster`, with the loop safety a lite cluster
link has.

## Plan

Until this lands, moq-transport peers are plain clients
([moq-transport peers are plain clients](/quest/m1/ietf-cluster-off.md)).

The extension must carry what a lite cluster link carries by then: the route
layer of the [cluster routing line](/quest/m1/cluster-routing/README.md)
(per-node ROUTEs with seqno and metric, path-less announces, the down-only
bit) and its selection, since cluster links are lite-only there.

Test: a moq-transport relay peering through the extension discards a route
that loops back to it, and resumes a subscription on another route when the
serving one dies.

Wire: extends `draft-lcurley-moq-cluster`, updated in the same PR.

## Required

- [Cluster routing](/quest/m1/cluster-routing/README.md) - settles what a cluster link carries
