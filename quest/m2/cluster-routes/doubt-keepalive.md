# [S] Doubt and keep-alive

## Goal

No relay moves onto a route through a relay that died silently, however far
apart its neighbours notice the death within the keep-alive.

## Plan

Reordered 2026-11-15 (maintainer, /quest-plan on [cache-aware
multicast](/quest/m2/multicast/README.md)): this waits on the
[bake-off](/quest/m2/multicast/bakeoff.md). The maintainer deferred how the
doubt covers a silent death on 2026-11-15, "not yet sure about the doubt
design", and decided the same day that evidence replaces the doubt. If the new
rule goes ahead, [the switch](/quest/m2/multicast/switch.md) deletes this
quest with the fixtures that pin it; if it does not, this starts as written.

If the doubt stays, the decision returns to the maintainer: (a) the doubt
lasts at least the configured idle timeout, so it covers the spread (the
recommendation: no wire change, and a relay already knows its idle timeout);
(b) relay keep-alives shorten below the doubt, which costs traffic on every
relay link; (c) accept the window and keep the test pinning it.

Kept 2026-11-15 (maintainer, /quest-complete on
[moq-dev/moq#8832](https://github.com/moq-dev/moq/pull/8832)): ✅ keep it in
m2 (rejected: drop; moving it to m3).

Found by [acceptance](/quest/m2/cluster-routes/acceptance.md) (epic Plan
item 30's first prior-art fixture):
`route_acceptance::a_vanished_relay_is_noticed_a_keep_alive_apart` kills
`X` silently, and its neighbours `R1`, `R2`, `R3` notice at 0 s, 1.5 s, and
3 s. With the 1 s doubt, `R1` moves onto a neighbour's route through the
dead `X` at 1 s, holds again at 1.5 s, and moves onto `R3`'s route through
`X` at 2.5 s; `R2` does the same at 2.5 s. All three go offline exactly once,
at 3 s. Readers see each move as available for a moment, and a request in
that window goes to a dead relay. A 1 s doubt does not cover a 3 s spread.

## Required

- [The bake-off](/quest/m2/multicast/bakeoff.md) - decides whether the doubt this sizes remains

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - Plan item 27's doubt and item 30
