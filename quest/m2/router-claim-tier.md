# [L] The router ranks one claim prefix's filings as one tier

## Goal

The lite-07 router (`rs/moq-net/src/route/relay.rs`) ranks every claim of one
prefix as one tier, wherever each is filed, as the origin and the moq-lite-07
draft's Resolution and Above text already do
([moq-dev/moq#9371](https://github.com/moq-dev/moq/pull/9371)). A request, a
forwarding step, and what a peer is told all follow the claim that tier's
ranking picks, so a reachable claim is never shadowed by an unreachable copy
of the same claim filed deeper.

## Plan

Found by the local review's Codex shadow on
[moq-dev/moq#9371](https://github.com/moq-dev/moq/pull/9371), which fixed the
origin and left the router, by the maintainer's pick taken while they were
away. The router still stops at the deepest filing: `Router::pick` takes the
first ancestor of the path the reader holds a claim at, and `step`, `awaits`,
`lead`, `shown_to`, and `stands` follow the same filing. Its doc says so
("reachable or not ... fails it closed"), so the behavior is by design, and
older than the origin fix, but it contradicts the draft.

Reproduction, as a router unit test: an endpoint's available claim of
instance 7 at `src`, and node 5's copy of the same claim filed at `src/a`
(Above 1). `pick((src/a/b, 7))` takes node 5 while its route lives. Once node
5's route dies, `pick` returns `None` and `step(me, ..)` returns `None`,
failing the request, though the endpoint's claim at `src` serves it. A peer's
narrow ANNOUNCE_REQUEST at `src/a` is likewise told only the unavailable
copy. The test fails on `main` and passes with the fix.

Until this lands, the origin and the router of one relay can disagree, which
a later review of that PR read from the source (not yet run): relay R owns
claim (`src`, I), and its reader also holds node P's copy shown at (`src/a`,
I, Above 1). Downstream D's origin picks R's cheaper claim for `src/a/b` and
names R, but R's `step` starts at `src/a`, finds no claim of its own there,
and refuses the request; before the origin fix, D named P. Reproduce it as a
two-relay test first. lite-07 is negotiated only by explicit opt-in
(`moq-lite-07-wip`), so this reaches only meshes that opt in.

Recommended approach: gather the claims of the most specific claim prefix
from every covering filing, as the origin's `best_route` and
`TableCursor::best` do, and rank them together by the draft's Resolution
order (not Unavailable, then not draining, then priority, then cost). That
reworks the router's told, gone, and ended bookkeeping, each keyed today by
the filing a claim is shown from, so a claim's filing moving between rows is
an in-place change rather than a withdrawal and a new claim.

Verify with the route rule's convergence proof as well as the tests, since
the change alters which claim a relay forwards to and advertises: update
`rs/moq-net/verify/route-algebra` (its claim rank) to rank a claim prefix's
filings as one tier and run `just test route-proof`. `/verify-warm` and
`/verify-failover` abstract routing to one broadcast and to any live upstream
winning, so neither sees a filing.

Put each run's result in the PR's Validation. No wire format changes; if the
draft's text needs more than conformance, the change lands in the lite-07
draft in the same PR.

## Related

- [Finalize moq-lite-07](/quest/m2/lite07-finalize.md) - requires this, so lite-07 does not freeze with the router contradicting its draft
