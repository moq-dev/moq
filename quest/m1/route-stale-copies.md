# [L] A prototype settles what keeps a route free of stale copies

## Goal

A test-only router beside `route::tests::Sim` runs lite-07 routing on paths
alone: a relay uses any route whose path does not name it, and a withdrawal
names the hop that died, so every route through that hop goes at once. The
same seeded meshes and scripts run on it and on today's router, which keeps
Babel's distance. The PR reports for both:

- whether selected routes ever loop, and for how many steps;
- for how many steps a relay still shows a route whose path is gone, after a
  link closes, a relay dies, and a publisher leaves;
- how many route messages each of those changes costs on 3, 6, 12, and 24
  relays;
- how many steps a relay with a longer way round takes to use it;
- whether a relay can pull a broadcast from a neighbour that already carries
  it and is no closer to the publisher than the relay.

The PR recommends one of the two rules, and the maintainer's pick is recorded
in the [epic](/quest/m2/cluster-routing/routes/README.md)'s Plan. Each bug the
reviews of [moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487) found
is pinned as a `residual_` test on today's router. The production router, the
wire, and the draft do not change here.

## Plan

Decided 2026-11-11 (maintainer, /quest-plan after #8487's two adversarial
reviews, revised the same day once the first plan's rule was tried on the
router). Moved here from the routes epic the same day (maintainer): the epic
and m2 wait on m1 quests this needs nothing from, it changes tests only, and
the m1 model quests wait on its outcome.

Why: today a relay believes a neighbour's copy of a publisher's claims only
while that neighbour is "feasible", which means strictly cheaper than the
lowest cost the relay ever advertised (Babel's distance). That set moves
whenever a cost or a seqno moves, and the relay's claims move with it. The
reviews found no loop and no stuck relay in route selection, and no remaining
bug on a mesh where every relay may see everything. Every finding needs
neighbours that know different parts of a publisher's claims, which happens
when a relay asks a neighbour for less than it wants because its grant on
that session is narrower. Call the prefix it asked for the **scope**.

What was tried, each as a few lines behind a switch on `main` at `688312b9eb`:

| | Today | Believe any neighbour whose route did not cross the relay | Believe a neighbour cheaper than the relay is now, ignoring the seqno, and keep what it sent |
| --- | --- | --- | --- |
| The seqno storm mesh settles | no | no | yes |
| A returning neighbour brings its claims back | yes | yes | yes |
| A seqno advance changes nothing shown | yes | yes | yes |
| Two relays behind a scoped relay let a dead claim go | yes | no, kept for good | yes |
| The blind relay shows a farther neighbour's claim | no | yes, as available, with no route to request it | no |
| The rest of `moq-net` | pass | 1 fails | 1 fails |

On lite-06 (`release`), over real sessions, the blind relay sees and resolves
the far claim, and the two relays retract the dead one once. lite-06 carries
a path on every announcement, so believing by path is sound there. lite-07
has one path per publisher, which cannot vouch for claims that reached the
sender another way.

Settled:

1. ✅ **A route is shared per publisher and per scope, and a relay believes a
   route by that route's own path.** Every claim on a route is then known
   completely for the route's prefix, and came along the path the route
   names. Where every relay may see everything this is one route per
   publisher, as today.
   [The model](/quest/m2/cluster-routing/routes/scope-model.md) and
   [the router](/quest/m2/cluster-routing/routes/scope.md) build it. Rejected:
   believing by the publisher's one path, the first plan, which the table
   shows is unsound. Rejected: believing by cost alone, the third column,
   which fixed the storm in 25 lines with no wire change; the maintainer does
   not want belief to rest on distance, and the blind relay stays blind.
   Rejected: a path per broadcast as lite-06 has, which re-announces every
   broadcast when a path changes.
2. ✅ **The blind relay is fixed by routing, not by showing more.** A relay
   whose closest neighbour is scoped does not show what a farther neighbour
   knows, and today that is consistent: it believes exactly the neighbours it
   can forward a request to, and the farther one is not feasible. With a
   route per scope the wider route is its own destination, so the relay can
   both show the claim and reach it.
3. ✅ **Only a broadcast some relay carries gets its own route, and that
   route propagates.** This reopens
   [cache-aware routing](/quest/m3/cache-routing/README.md)'s one-hop warm
   route (its Plan item 8).
4. ✅ **What keeps a route free of loops and stale copies is this quest's to
   measure, not yet decided.** The hops stop loops, not stale copies.
   Two candidates:
   - **Distance, as today.** A relay uses a neighbour only when the
     neighbour's cost is below the lowest cost the relay ever advertised.
     Anything routed through the relay costs more than that, so such a
     neighbour's route is no copy of the relay's own. It can still be stale
     another way: a neighbour whose path closed stays usable until its
     withdrawal arrives. It never loops, even for a moment. It refuses
     good routes: a longer way round waits for the publisher to advance its
     seqno, and a relay cannot join a copy carried by a neighbour that is no
     closer to the publisher, which is the join cache-aware routing wants.
     The seqno, ROUTE_REQUEST, ROUTE_HOLD, and the ROUTE_END echo all exist
     to recover what this check refuses.
   - **Paths alone, with a withdrawal that names what died.** A relay uses
     any route whose path does not name it. Each hop in a path names the
     relay and the session it learned the route on. When that session
     closes, the relay's withdrawal names that hop, and every relay drops
     every route through it at once, whichever neighbour offered it. This is
     root cause notification from BGP research
     ([Pei et al. 2005](https://www.cs.colostate.edu/~massey/pubs/journal/massey_comnet05.pdf)),
     which bounds convergence by the mesh's diameter. Its known gap: when a
     relay with several neighbours dies, each neighbour reports its own
     session, so one stale switch per neighbour can remain. A route can also
     loop for a moment while a path change is in flight. It needs no seqno,
     request, hold, or echo. lite-06 instead holds stale paths back with a
     300 ms hold and a 2 s quarantine, and
     [moq-dev/moq#5375](https://github.com/moq-dev/moq/pull/5375) still
     measured stale revivals.

   The October measurement that chose Babel compared it with a path per
   broadcast, not with shared routes, so its message counts do not answer
   this.
5. ✅ **No routes to relays.** A relay keeps the cheapest path to each
   publisher's scope, not to a relay that publisher is attached to.

Work:

- Write the paths-alone router as test-only code under `route/tests/`, as
  small as it can be: routes shared per publisher, each with a path and a
  cost; the withdrawal that names a hop; and claims that ride a route with
  one floor, newest floor winning. Leave out what the comparison does not
  need: scoped grants, qualified keys, interest changes, GOAWAY, and RTT.
- A relay remembers a dead hop only while one of its neighbours still offers
  a route through it. Report whether a stale route can return after that,
  and for how long.
- Drive both routers from one script generator, the one behind
  `sweep_no_route_outgrows_a_loop_free_path`, and from the mesh shapes of
  `rs/moq-net/tests/integration/mesh_sweep.rs`'s benchmark. Check at every
  step and at every quiet point, and count as the Goal lists.
- Report what a relay's reader sees when its only route goes. Today it holds
  the claims as unavailable while a seqno request is out
  ([epic](/quest/m2/cluster-routing/routes/README.md) Plan item 19); on
  paths alone there is nothing to wait for, so it lets go at once.
- Build the cached-copy case for both: a relay whose neighbour carries a
  broadcast and is farther from the publisher. Two relays that each carry a
  copy must not adopt each other, and a copy that goes must take every route
  through it along.
- Pin each finding as a `residual_` test in `route/tests/`, asserting what
  today's router does, with a comment naming
  [the router quest](/quest/m2/cluster-routing/routes/scope.md): the seqno
  storm ([reproducer](https://github.com/moq-dev/moq/pull/8487#issuecomment-6798722861)),
  the forgotten claim and the two tests its failed fix broke
  ([reproducers](https://github.com/moq-dev/moq/pull/8487#issuecomment-6802083874)),
  and the blind relay: a publisher behind a relay that asked for `a` and
  behind a two-relay chain that asked for everything, both joined to one
  relay, which shows `a/x` and not `b/x`.
- Add the dead claim as a plain test, since today's router passes it and any
  new rule must: a relay whose only way to the publisher asked for `a`, with
  two relays joined to it and to each other, one of which also has a dear
  link straight to the publisher. That link closes and the publisher
  withdraws `b/x`; neither relay may still show it.
- Record the maintainer's pick in the epic's Plan item 26 and in
  [Cluster routing](/quest/m2/cluster-routing/README.md)'s "Keep Babel"
  bullet. If the pick is paths alone, plan removing the distance, the seqno,
  ROUTE_REQUEST, ROUTE_HOLD, and the echo with `/quest-plan` before the
  router quest starts.

Public API: none. Wire: none.

## Related

- [The route model has a route per scope](/quest/m2/cluster-routing/routes/scope-model.md) - models the rule this picks
- [Cache-aware routing](/quest/m3/cache-routing/README.md) - its warm routes rest on the rule this picks
