# [M] Route benches on the router

## Goal

`route::relay::Router` has benches for what the old path rule measured, swept
over both axes, and the costs the switch measured and left are fixed or kept
with a number that says why.

## Plan

Found by [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) and
proposed by [moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053).

Decided 2026-11-18 (maintainer, on
[moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053#issuecomment-6975349236)):
✅ kept (rejected: dropping it). It follows the lite-07 cut, as the
2026-11-17 brief allowed for benches, so it sits in m3 rather than the
multicast epic the cut waits on.

- The path-rule benches (drain, repath, follow) left `benches/routes.rs`
  with the old router and have no equivalent on the new one. Add them,
  swept over both axes: relays on the path and broadcasts per node.
- Measure each cost the switch left, then fix it or record why it stays:
  - re-offering a route sends its node's claims again;
  - a non-routing peer is told one ANNOUNCE_UPDATE per broadcast when a
    route's cost changes;
  - an edit swapping one prefix for another tells again the claims both
    cover;
  - a relay whose only route closes ends its subscriptions while the claim
    stays shown.
- [moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053) sends a
  whole copy of a node's claims wherever its filtered copy used to go
  (`withheld` is gone), so a claim moving between a node's two identities
  costs a revision under each. Bench a churn of adoptions over a marked
  link with it.

Public API: none. Wire: none, unless a fix changes what a claim carries,
which lands before the lite-07 cut.

## Related

- [Cache-aware multicast](/quest/m2/multicast/README.md) - the route layer these measure
- [What the route switch left](/quest/m2/multicast/switch.md) - where these came from
