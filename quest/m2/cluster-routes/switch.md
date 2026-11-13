# [XL] lite-07 sessions run the path router

## Goal

Every lite-07 session, in Rust and in `js/net`, routes by the rule of the
[epic](/quest/m2/cluster-routes/README.md)'s Plan item 27, on the
router [the route per scope](https://github.com/moq-dev/moq/pull/8622)
built (`route::path::Router`). A route on the wire carries its path with a session in each hop and
the prefix it covers. A relay that loses a session names that hop in one
message, and a relay answers a neighbour that holds for it. Babel's
distance, the seqno, ROUTE_REQUEST, and the ROUTE_END echo are gone from
lite-07-wip, the draft, both codecs, and the tree: today's router is
deleted, and one route rule remains.

A detour no longer waits on the publisher, a relay that dies costs one
withdrawal per session in place of a seqno round, and the draft and the
epic's contract say what the sessions do. lite-06 and older and
moq-transport are untouched.

## Plan

Decided 2026-11-12 (maintainer, /quest-plan on the quests still written for
Babel):

- ✅ The removal is two quests in this epic: the router first with no wire
  change, then this switch (rejected: the router quest in m1; one quest for
  everything).
- ✅ The switch lands on today's streams, before
  [the route stream](/quest/m2/cluster-routes/route-stream.md)
  (rejected: the route stream first, which builds the echo and ROUTE_REQUEST
  on the new stream and then deletes them; one wire change for both, which
  joins two XL quests and waits on the interest set). The path rule sends no
  routing message on the subscriber half, so the ordering the route stream
  was planned to give the echo is not needed here.

Recommended wire, for the implementer to confirm or improve in the PR; each
departure is a choice to list there. Where the model settled something
else, the model wins:

- ROUTE_START and ROUTE_UPDATE carry the path as hops, each a Node ID and
  the session that node learned the route on, and the prefix the route
  covers as the model settled it. The seqno and the down-only bit go. The
  metric stays as the cost that ranks routes, and no longer decides which
  are safe. The hop limit stays the wire's 32.
- One message names a dead hop. A relay sends it once per session it loses,
  to every neighbour, covering every publisher, and it lists the routes that
  moved to another path, as the prototype has it.
- The answer that ends a hold rides the publisher half: the stream that
  carries the answering side's routes to the relay that waits. It is then
  ordered behind every route its sender sent first, on today's streams. A
  relay waits only on a neighbour it listens to, since no other can hand a
  stale route back.
- ROUTE_HOLD stays as the model has it (epic Plan items 14 and 19).
- ROUTE_REQUEST and the echo are removed with their types. The Node ID a
  request names (epic Plan item 2) stays.
- The doubt is local: nothing on the wire carries it. Its timer is the
  router's, settled by the model and the sweeps from the maintainer's 1 s.

Work:

- `route/node.rs` drives the new router. `home` serves a request through
  the route the rule picks, in place of every feasible route whose neighbour
  `carries` the claim.
- Delete today's router with what only it used: the feasibility distance and
  its drain, seqno requests and their pending state, the echo, and their
  fixtures. A fixture that states a claim rule moves to the new router if
  [the route per scope](https://github.com/moq-dev/moq/pull/8622) did not
  move it already.
- The Rust codec and sessions (`rs/moq-net/src/lite/`), and `js/net`'s route
  layer (`js/net/src/lite/publisher.ts`, `js/net/src/lite/subscriber.ts`,
  `js/net/src/lite/floors.ts`, `js/net/src/route.ts`) in the same PR. A
  browser is an endpoint: it sends its own route, refuses a route whose path
  names it, and applies a withdrawal that names a hop.
- Make js/net's origin apply the covering rule (`covering` in
  `route/view.rs`) when it picks among a session's routes, as its consume
  cache does through `Floors.covers`. Decided 2026-11-12 (maintainer, on
  [moq-dev/moq#8443](https://github.com/moq-dev/moq/pull/8443)): ✅ leave it to
  this rewrite (rejected: port the per-node merged view in #8443). Until then,
  when a newer floor withdraws a node's claim above `live/x` while another
  node claims `live` with a different instance, `forwardAnnounced` still
  offers the withdrawn claim, and an origin request for `live/x` fails
  `Unroutable` until the lagging stream's floor lands. Recommended: the
  origin asks a session's route, per request, whether it covers the
  requested path (the session's `Floors.covers`) and skips one that does not,
  since an announcement at `live` cannot say it stops short of `live/x`.
  Test it with the two-stream, two-instance case from #8443's review.
- [moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457)'s warm-route
  sweep (`route/tests/cache.rs`) and `Router::eligible` rest on the
  feasibility distance. The reopened
  [cache-aware routing proof](/quest/m2/cache-routing-proof.md) rewrites them
  for the path; if it has not landed, move the sweep onto the new router's
  query here and say what it no longer checks.
- Draft: Routing states the path rule, the withdrawal, holds, and the doubt.
  Floors says a floor certifies its route's prefix, and the condition "every
  peer whose last advertised route to it is feasible here has answered the
  current request" becomes the settled rule. Remove ROUTE_REQUEST and the
  echo from the message sections and the stream tables, and add a changelog
  bullet.
- Rewrite the epic's contract as built, mark its Plan items 3, 7, 20, and
  the echo and drain of item 25 as superseded by item 27, and update the
  `verify-route` skill's rules, coverage, and sync commit.
- Tests on real sessions with mocked time: a detour that avoids the far end
  of a closed link is taken at once, and one through it after the doubt; a
  relay with several neighbours that dies reaches every relay as withdrawals
  and no relay moves onto a route through it early; a hold ends behind a
  neighbour that answers and behind one whose session closes; an unknown
  hop named in a withdrawal changes nothing; a path at the hop limit is
  refused.
- Measure route and claim messages per change with
  `rs/moq-net/tests/integration/mesh_sweep.rs`'s benchmark before and after,
  and put both in the PR.
- Run `just check`, `just test interop --all`, and `just drafts check`.

Rollout: lite-07 is `-wip`, so a relay on this wire refuses one on the old
by misparse, not by version. Relays pinned to `main` roll together, as each
earlier lite-07 wire change did.

Public API: none expected. Wire: lite-07-wip only.

## Required

- [Settle the path router's remaining design choices](/quest/m2/cluster-routes/scope.md) - the maintainer's policy choice and its model checks

## Related

- [Each side says everything about routing on one stream](/quest/m2/cluster-routes/route-stream.md) - moves these messages onto one stream per side afterwards
- [Route layer acceptance](/quest/m2/cluster-routes/acceptance.md) - proves the switched sessions end to end
