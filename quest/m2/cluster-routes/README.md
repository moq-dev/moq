# Routes and announces

## Goal

Routing info travels in two layers on lite-07's Announce Stream, for every
session: a ROUTE advertises reachability of a routing node, and a claim says
a prefix is served there. A link flap or relay loss sends route changes per
routing node, never a re-announce per broadcast; no stale copy revives a
claim whose withdrawal is already known; and a subscription moves between
claims of one Instance without a restart. The route layer is accepted on
real sessions, and the lite-06 churn mitigations it replaces are retired.

## Plan

Superseded 2026-11-15 (maintainer, /quest-plan, item 31 of the earlier Plan):
[cache-aware multicast](/quest/m2/multicast/README.md) replaced this epic's
path rule on lite-07-wip before the cut, and
[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) shipped the
replacement. Gone with the old router: every publisher an origin node (item
18), ROUTE_HOLD (items 14 and 19), routes per scope (item 26), the doubt and
remembered dead hops (item 27), the Route Key and qualified Node IDs, and
the handoff contract's holds. The earlier Plan, items 1 to 31 with the
routing contract, the handoff contract and its fixtures, and the simulator
findings, is in `git show 4e73451999:quest/m2/cluster-routes/README.md`.

Standing from it, as item 31 listed and the lite-07 draft carries them: a
route's path with its self-naming rule and the withdrawal that names a dead
hop (DEAD); revisions and floors, so the newest copy of a node's claims
wins; one Announce Stream per side with its interest set edited in place
and SYNCED per prefix (items 21 to 24); the sticky down-only flag (item 29),
now a route's mark; and drain, then priority, then cost. A request names the
routing node its first hop chose, and one naming none lets the publisher
select (item 2).

Why not a hold-down: [moq-dev/moq#4644](https://github.com/moq-dev/moq/pull/4644)
retracted at once and re-announced a replacement after 1 s, which ended a
downstream front whose only path ran through the relay, dropping
subscribers for about a second while the relay kept serving; holding
without retracting brought hunting back in measurements, and #4642's 300 ms
cursor hold was the earlier mitigation. Main also contains
[moq-dev/moq#5375](https://github.com/moq-dev/moq/pull/5375)'s two-second
retraction quarantine and
[moq-dev/moq#5391](https://github.com/moq-dev/moq/pull/5391)'s same-clock
queue dedup. #5375 measured residual stale revivals, so neither longer holds
nor one passing mesh establish correctness. Retire the mitigations only
after the incident fixtures and failover tests demonstrate their
replacement, as [acceptance](/quest/m2/cluster-routes/acceptance.md) does.

## Related

- [Route wakes](/quest/m4/route-wakes.md) - the origin's side of a route change
