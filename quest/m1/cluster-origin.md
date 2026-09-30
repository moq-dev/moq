# [M] Cluster origin reply

## Goal

A relay that spreads a pool's paths never lets a moq-transport downstream
splice one pool member's content onto another's, and
`draft-lcurley-moq-cluster` says how a downstream learns which member serves a
path.

## Plan

Spreading keys route selection's hash on the requested path, so equal-cost
advertisers of one prefix share its paths, and a relay advertises one route for
the whole pool. On moq-lite-07 the SUBSCRIBE_OK and FETCH_OK replies name the
serving origin, and a relay splices a failover only between replies naming the
same one. The cluster extension (moq-transport 17+) has no such field, so a
downstream relay still pins failover to the advertisement's first Hop ID, which
labels the pool rather than the member serving the path. The draft's
"Several Publishers of One Namespace" section still requires that the first
Hop ID downstream name the publisher whose Objects flow, which a spreading
relay breaks.

Two ways out, to settle before implementing:

- Name the serving origin in a moq-transport reply as a cluster-extension
  parameter, mirroring lite-07, and stitch on it. Recommended: one identity
  model on every wire.
- Keep the label truthful instead: toward a downstream that cannot learn the
  origin, do not spread (or advertise per path).

Either way the test is the one lite-07 has: two workers behind a pool relay
behind a moq-transport downstream relay, and killing the serving worker ends
the downstream subscription rather than splicing the survivor's objects.

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - spreading and the lite-07 reply
  origin landed there
