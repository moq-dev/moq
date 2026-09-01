# [L] Route resume identity

## Goal

A subscription whose serving route goes away resumes through the next best
route without the consumer noticing, as it did before announcements became
prefix routes. A route change that preserves content identity re-splices at a
group boundary; one that does not ends the subscription so the consumer starts
a new one.

This restores behavior that shipped, and it supplies the per-path identity two
other questlines were built on top of.

## Plan

### Root cause

[moq#3225](https://github.com/moq-dev/moq/pull/3225) made announcements prefix
routes and moved route state out of `broadcast::Broadcast` into `origin`. A
front used to hold the routes for its path and the identity of whoever was
serving it: `FrontState.publisher` was the first hop of the best route, and
`same_identity` decided whether an arriving route joined the existing front
(splice) or started a fresh broadcast. Both are gone. Fronts on `dev` hold
local sources only, so `FrontState::reselect` picks between two
`create_broadcast` calls in one process and nothing spans sessions.

The PR calls the loss deliberate, on the grounds that cross-session splicing
relied on content identity that per-broadcast announcements carried. That is
half right: `Epoch` is gone, but the first hop is not. Hop chains still travel
on the wire, `RouteEntry` still stores them, and `route_order` still ranks on
them. What was lost is the structure that read the first hop, not the value.

So failover today is abort and resubscribe: `origin::Consumer::routed_broadcast`
retries, and every reader sees the subscription end.

### What to restore

Give the per-path serve state an identity, derived from the first hop of the
route currently serving it, and re-splice rather than abort when a route change
preserves it:

- Equal identities are the same publisher reached another way (a reconnect, a
  cheaper path, a relay restarting). Splice at a group boundary.
- Different identities are different content at one path. End the subscription
  rather than splicing one publisher's subscribers onto another's frames.
- `Hop::UNKNOWN` identifies nobody and never matches itself, which is what
  `same_identity` existed to encode. Two anonymous peers must not pass for one
  reconnecting.

An empty hop chain is a local publish and keeps the local-source behavior it
has now.

### Boundaries

Identity here is a routing-layer property, not a media one. It answers "is this
the same source" well enough to splice a live subscription, and deliberately
does not try to answer "are these bytes interchangeable", which is what
`Epoch` was reaching for and what
[plan-hls-identity](/quest/m2/plan-hls-identity.md) still owns for cacheable
URLs.

The fix lands on `dev`, where the regression is; `main` has no prefix routes.

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
