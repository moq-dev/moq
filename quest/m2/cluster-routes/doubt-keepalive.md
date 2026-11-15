# [S] Doubt and keep-alive

## Goal

No relay moves onto a route through a relay that died silently, however far
apart its neighbours notice the death within the keep-alive.

## Plan

Found by [acceptance](/quest/m2/cluster-routes/acceptance.md) (epic Plan
item 30's first prior-art fixture):
`route_acceptance::a_vanished_relay_is_noticed_a_keep_alive_apart` kills
`X` silently, and its neighbours `R1`, `R2`, `R3` notice at 0 s, 1.5 s, and
3 s. With the 1 s doubt, `R1` moves onto a neighbour's route through the
dead `X` at 1 s, holds again at 1.5 s, and moves onto `R3`'s route through
`X` at 2.5 s; `R2` does the same at 2.5 s. All three go offline exactly once,
at 3 s. Readers see each move as available for a moment, and a request in
that window goes to a dead relay. A 1 s doubt does not cover a 3 s spread.

## Waiting

- Maintainer: how the doubt covers a silent death. Options: (a) the doubt
  lasts at least the configured idle timeout, so it covers the spread (the
  recommendation: no wire change, and a relay already knows its idle
  timeout); (b) relay keep-alives shorten below the doubt, which costs
  traffic on every relay link; (c) accept the window and keep the test
  pinning it. Check with `/quest-grill` or the PR that picks it up.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - Plan item 27's doubt and item 30
