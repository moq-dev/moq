# [XL] Route resume identity

## Goal

A subscription whose serving route goes away resumes through the next best
route without the consumer noticing, as it did before announcements became
prefix routes. A route change that preserves the origin identity re-splices at
a group boundary; one that does not ends the subscription so the consumer
starts a new one.

This supplies the per-path identity two other questlines are built on top of.

## Plan

### Root cause, and the decision

[moq#3225](https://github.com/moq-dev/moq/pull/3225) made announcements prefix
routes and moved route state out of `broadcast::Broadcast` into `origin`. A
front used to hold the routes for its path and the identity of whoever was
serving it: `FrontState.publisher` was the first hop of the best route, and
`same_identity` decided whether an arriving route joined the existing front
(splice) or started a fresh broadcast. Both are gone, and the PR specified the
loss into the draft: "A route carries no content identity", and a relay "MUST
NOT splice a live subscription across sources reached through different
routes". Failover on dev is abort and resubscribe
(`origin::Consumer::routed_broadcast` retries), and every reader sees the
subscription end.

Decision (2026-09-01): that spec point is reversed. Routes keep carrying the
hops and cost of what they serve, and a relay resumes/stitches a broadcast
across routes when the first hop is the same. `Epoch` stays dead; it was a
per-broadcast content generation, and nothing per-broadcast survives prefix
routes. What identifies a source is the route's first hop, which is still on
the wire, still in `RouteEntry`, and still ranked on by `route_order`. What was
lost is the structure that read it, not the value.

### What to restore

Give the per-path serve state an identity, derived from the first hop of the
route currently serving it, and re-splice rather than abort when a route change
preserves it:

- Equal non-zero first hops are the same origin endpoint reached another way (a
  reconnect, a cheaper path, a relay restarting). Splice at a group boundary.
- Different first hops are different sources at one path. End the subscription
  rather than splicing one source's subscribers onto another's frames.
- `Hop::UNKNOWN` identifies nobody and never matches itself, which is what
  `same_identity` existed to encode. Two anonymous peers must not pass for one
  reconnecting.

An empty hop chain is a local publish and keeps the local-source behavior it
has now.

This is more than restoring a field, which is why the quest is sized XL. On dev
`ServeState` is per-announcer, and `create_source` mints a broadcast that is
deliberately not inserted into the broadcast tree, so nothing per-path spans
routes. The work includes rebuilding that front: a per-path structure that
outlives any single route, holds the serving identity, and is what a
replacement route splices into.

### Draft

The implementing PR amends `drafts/draft-lcurley-moq-lite.md` in the same
change, per the wire-behavior rule. The "route carries no content identity"
paragraph becomes the first-hop rule: two routes covering one path with the
same non-zero first hop are the same origin reached different ways, and a
relay MAY move a live subscription between them at a group boundary; across
differing or unknown first hops a relay MUST NOT splice, and the subscription
ends with the serving session as it does today. Record the change in the
moq-lite-06 changelog.

### Boundaries

Identity here is a routing-layer property, not a media one. The first hop
identifies the endpoint that originated the route, which for a prefix route is
whoever serves the subtree; equal first hops mean the same endpoint, not that
two encodings are interchangeable. That media-generation question is what
`Epoch` was reaching for and what
[plan-hls-identity](/quest/m2/plan-hls-identity.md) still owns for cacheable
URLs.

The fix lands on `dev`, where prefix routes live; `main` has no prefix routes.

### Tests

The pre-#3225 goaway and cluster tests are the specification: a draining
session serves through the handover window and its subscribers move to the
surviving route without the subscription ending. Add the first-hop cases the
old front covered: a reconnecting publisher splices, a different publisher at
the same path does not, two anonymous peers never pass for one, and a
metadata-only reprice changes nothing visible.

## Related

- [Rank](/quest/m2/pop-skipping/rank.md) - reuses this comparison rule on the
  route's *last* hop, since adopting a parent is about the adjacent relay rather
  than about who produced the content
- [Resolve](/quest/m2/wildcard/resolve.md) - two publishers colliding at one
  literal path resolve through this identity
- [plan-hls-identity](/quest/m2/plan-hls-identity.md) - the media-generation
  identity this deliberately does not supply
