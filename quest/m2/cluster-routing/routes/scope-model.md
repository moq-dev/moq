# [L] The route model has a route per scope

## Goal

`rs/moq-net/verify/route/RouteHandoff.tla` gains two prefixes and relays that
ask a neighbour for only one of them. A route is keyed by its publisher and
the prefix it covers, carries its own path, and is believed by a relay it did
not cross. TLC shows on it:

- A relay shows a claim exactly while it holds a route that covers the claim
  and did not cross the relay, at the newest revision the relay knows.
- Once nothing is in flight, no relay shows a claim that no path from its
  publisher carries.
- A change of path or seqno alone starts, ends, and changes no claim a reader
  sees. Whether the claim is available may change.
- A request for a shown claim follows next hops to the publisher without a
  loop, also where one route's prefix lies inside another's. So a relay
  behind a scoped neighbour reaches what a farther, unscoped neighbour
  offers.

The model uses the rule
[the prototype](/quest/m2/cluster-routing/routes/stale-copies.md) picked for
keeping a route free of stale copies. The rule this settles is written into
the epic's contract for
[the router](/quest/m2/cluster-routing/routes/scope.md) to implement. The
router's behaviour, the wire, and the draft do not change here.

## Plan

Decided 2026-11-11 (maintainer, /quest-plan). The decisions and what was
tried are in [the prototype](/quest/m2/cluster-routing/routes/stale-copies.md)'s
Plan and the [epic](/quest/m2/cluster-routing/routes/README.md)'s Plan item
26.

Why a route per scope: a relay that asked a neighbour for prefix `a` knows a
publisher's claims only inside `a`. Today that partial knowledge lives inside
one view per publisher, as a floor per region, with claims no floor covers
carried loose, and a rule for which neighbours vouch for which region. Every
open finding is in that machinery. With the prefix on the route, every
neighbour that offers a route knows all of it, and the route ends when its
path does.

Recommendations the model may overturn. Say in the PR what it settled on and
why:

- **The prefix a route covers travels with the route**, as a field of
  ROUTE_START. On a stream that asked for a narrower prefix the route covers
  that prefix, so narrowing composes. Rejected so far: deriving a Node ID
  from the publisher and the prefix, which needs no wire change but cannot
  be narrowed a second time, since a relay cannot read the prefix back.
- **The newest floor wins among one publisher's routes.** A claim both a
  narrow and a wide route cover is shown once, at the newer revision, and a
  relay keeps what it learned while any route covers the claim
  (epic Plan item 19). Nothing is pruned by an instant: a route that ends
  takes only what no other route covers.
- **A request takes the narrowest route that covers its claim, and is never
  forwarded along a route whose path names the neighbour it came from.**
  This is the unsolved part: prove the selected next hops cannot loop when
  the narrow and the wide route lead different ways, under stale messages,
  or find the rule that does.
- Keep the counterexample that broke the first plan: two relays behind a
  scoped relay must not keep a wider route alive through each other after
  its path closed.
- Keep the lagging neighbour: a relay learns a withdrawal from one route,
  that route ends, and a second route still carries the claim at the older
  revision. The relay must not show the claim again
  (`an_older_complete_snapshot_revives_no_known_withdrawal`). Give it a
  config, with the second route covering the same prefix and a wider one.

Work:

- In the model, replace the fixed root interest with two prefixes and a
  `Scoped` set of relays that ask for one. Key `rx`, the selection, and what
  is sent by route. Replace `Certifiers`, `Certified`, `Prune`, and the
  stored merged `view` with one floor per route and the newest-floor rule.
  Add the Goal's four rules: `Seen`, an extended `QuietIsSettled`, `Stable`,
  and `Reaches`.
- Add configs for the dead claim, the blind relay, and the meshes in the two
  reproducer comments the prototype pins, at bounds that finish in minutes on
  the recipe's settings. Run TLC one config at a time, simulation first.
- Rewrite the epic's bullet that begins "A node tracks the regions each
  neighbour's last view certifies" to the settled rule, marked as not yet
  built.
- Update the `verify-route` skill's rules table, coverage table, and
  "Not covered" list.

[The interest set](/quest/m2/cluster-routing/routes/interest-set.md) adds
edits to the two prefixes this introduces.

Public API: none. Wire: none.

## Required

- [A prototype settles what keeps a route free of stale copies](/quest/m2/cluster-routing/routes/stale-copies.md) - the rule for loops and stale copies that this model is written on

## Related

- [The router keeps a route per scope](/quest/m2/cluster-routing/routes/scope.md) - implements the rule this settles
