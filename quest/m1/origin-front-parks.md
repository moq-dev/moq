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

Decided 2026-10-08: benchmark first. Land a benchmark swept over requesters
and route-table churn, and build the parked front only if the re-mint shows
as a real slope; the same benchmark then shows the replacement is cheaper. A
measured no-win deletes this quest. The `origin/viewer_*` benches do not
cover the retry loop.

The filtered front a peer session leaves behind, folded in here on
2026-10-05, moved to [Idle fronts](/quest/m0/idle-fronts.md) on 2026-10-07:
it ends once unread like any other front.

Public API: no signature change expected. `routed_broadcast` and
`request_broadcast` keep their contracts; only where the waiting happens
changes.

Fronts are keyed by effective exclusion (`Horizon::effective`), so viewers
share them.
