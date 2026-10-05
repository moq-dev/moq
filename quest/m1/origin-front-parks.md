# [S] An unroutable request waits on the front, not in a retry loop

## Goal

`origin::Consumer::routed_broadcast` retries: it registers a watch on the
path, asks, and on `Unroutable` waits for the covering routes to move. The
wait lives beside the fronts the driver runs rather than in them, so a path
nothing covers is re-asked from scratch on every table move, minting and
tearing a front down each time. A request for an unroutable path should be
able to mint a front that waits for coverage, and the loop should go away.

## Plan

The obstacle, and the reason #3901 rejected this as an alternative, is that
fronts are shared per path. A front that parks would park every requester at
that path, including `request_broadcast`, whose whole contract is a verdict
now. So the disposition has to travel with the requester rather than the
front: `request_broadcast` resolves the current verdict at once without
parking, while `routed_broadcast` stays registered until its route completes.
The front ends only once every parked requester has been handled, so one
requester's disposition never ends another's wait.

Worth confirming before building it: that the retry loop costs something. A
benchmark swept over requesters and route-table churn would show whether the
re-mint is a real slope or noise, and it is the same benchmark that would
show the replacement is cheaper.

Public API: no signature change expected. `routed_broadcast` and
`request_broadcast` keep their contracts; only where the waiting happens
changes.

Decided in the 2026-09-30 audit: this lands after shared fronts, which
reworks the same `model/origin.rs` fronts, and may move to m2 if shared
fronts' benchmark shows the retry loop's re-mint is noise.

## Required

- [Wildcard](/quest/m0/wildcard/README.md) - `routed_broadcast`, `request_broadcast`, and the pool code this reworks exist only on its line branch
- [Shared fronts](/quest/m0/shared-fronts.md) - reworks the same fronts, and its benchmark decides whether this stays in m1
