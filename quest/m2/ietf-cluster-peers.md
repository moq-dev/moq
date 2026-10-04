# [L] moq-transport cluster peers

## Goal

A moq-transport relay joins a cluster as a peer through an extended
`draft-lcurley-moq-cluster`, and never splices one pool member's content onto
another's.

## Plan

Replaces the wildcard line's "Cluster origin reply" quest.

The extension must carry what a lite cluster link carries by then:

- The serving origin in the reply, mirroring lite-07's SUBSCRIBE_OK and
  FETCH_OK Origin, so a downstream stitches failover on the member serving the
  path rather than the advertisement's first Hop ID. The draft's "Several
  Publishers of One Namespace" section changes with it.
- The route layer of the [cluster routing line](/quest/m1/cluster-routing/README.md)
  (per-node ROUTEs with seqno and metric, path-less announces, the down-only
  bit) and its selection.

Test: two workers behind a pool relay behind a moq-transport downstream relay;
killing the serving worker ends the downstream subscription rather than
splicing the survivor's objects.

Wire: extends `draft-lcurley-moq-cluster`, updated in the same PR.

## Required

- [Cluster routing](/quest/m1/cluster-routing/README.md) - settles what a cluster link carries
