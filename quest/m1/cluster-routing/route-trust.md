# [M] Route trust

## Goal

A session may advertise routes only to nodes it is trusted for, so an
authorized publisher cannot claim a cheap route to a CDN relay's node and
attract its subscriptions. Cluster-peer links may advertise any node. A
client link's node ids stay scoped to that session, unless its peer grant
names the nodes it may speak for: then a Starlink drone carries its fleet's
nodes as their real identities, and two uplinks of one swarm reach one origin
instead of two unrelated ones.

## Plan

Decided 2026-10-01:

- Session scoping is the default and ships with
  [Routes and announces](/quest/m1/cluster-routing/routes.md): a scoped id
  resolves only that session's own authorized announces and never shadows a
  route learned on a trusted link.
- Promotion rides [Peer grants](/quest/m1/auth/peer-grant.md), the
  hop-bound credential a direct session already presents; extend it to name
  the node ids (or an id range or prefix of them) the holder may advertise.
  Decide the shape with the auth line.
- A promoted link is still subject to its upstream attribute and the
  down-only bit.

Test: a client advertising a cheap route to a cluster node does not move any
other session's subscription; two uplinks of one mesh holding grants for the
same drone node fail over between each other at a group boundary instead of
ending the subscription.

Public API: the grant's node scope (`moq-token`, `js/token`, and the token
CLI docs). Wire: none beyond the grant.

## Required

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the node ids this scopes
- [Peer grants](/quest/m1/auth/peer-grant.md) - the credential this extends
