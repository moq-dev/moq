# [L] A node never takes back a route that crossed it

## Goal

A lite-07 route names the relays it crossed, and a node never uses a route
that names itself. A node that has forgotten an identity therefore cannot
take back a copy of its own old route, however long that copy was in flight
and whatever sessions closed meanwhile. The router, `route::tests::Sim`, the
route model, and the draft agree on the rule, and TLC finds no count to
infinity on meshes where the origin sits behind a relay, with unit and with
unequal link costs, with the echo travelling as it does on today's streams.
A dead publisher goes offline everywhere and routing goes quiet.

## Plan

Found 2026-11-10 by the route model ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)):
`rs/moq-net/verify/route/CountToInfinity.cfg` breaks `NoCountToInfinity` in
9 steps with no partition. A node drops its feasibility distance once every
neighbour echoed its ROUTE_END (`update` in `route/mod.rs`). But the echo
rides the announce stream the node answers, while the neighbour's own routes
ride the stream the node asked for (`route/node.rs`), so the echo can
overtake an older route the neighbour derived from the node's.

Found the same day, a /quest-plan on lite-07 routing: **putting the echo in
order is not enough.** With the echo behind the echoer's routes, as
[moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487) has it in the
model and `Sim`, the model still counts to infinity once the origin is not
adjacent to every relay. An echo proves only that nothing derived is in
flight on that one link; a derived copy can sit at a third relay and arrive
over two hops. The shortest case, an origin behind relay A with A, B, and C
in a triangle and unequal costs:

1. The origin announces. A advertises to B and C, and B and C to each other.
2. The origin's session closes. A sends ROUTE_END to B and C.
3. B and C each apply it and echo. Their earlier ROUTEs to each other are
   still queued.
4. A has both echoes and drops its distance.
5. B receives C's old ROUTE, which is feasible there, selects it, and
   advertises it to A, which accepts it.
6. The route laps A, C, B, A, each relay draining and accepting again, and
   the metric climbs without bound.

#8487's triangle passes because its origin is one of the three nodes and
drops what it hears for its own identity.

Decided 2026-11-10 (maintainer, the same /quest-plan), asked how a relay
safely forgets a publisher that is gone: a relay acknowledges a ROUTE_END
only when its own downstream has. **Superseded the same day, after the model
ran it.** That rule needed three sub-rules, each forced by a counterexample
(a held route counts as routing through the sender; a holding node defers
until the hold resolves; a deferred echo waits only on the ROUTE_ENDs
outstanding when it was deferred). It then passed with one flap and failed
with two: a closed session counts as that neighbour's echo while its last
advertisement is still in flight, so a relay takes back one stale copy of
its own route. The copy stayed within two laps, but no rule that works by
proving "nothing is left" survives a session that closes before answering.
It also makes an upstream wait on its whole subtree before it may
re-advertise, and lets a peer that withholds its echo stall that upstream.

Decided 2026-11-10 (maintainer, the same /quest-plan, with the model results
in hand): ✅ **each route names the relays it crossed, and a node never uses
a route that names itself.** Feasibility still selects routes and still
stops hunting; the list is only a refusal, and it covers the one case
feasibility cannot, a node that no longer remembers. It is one list per
origin route, not the per-claim Hop ID list lite-07 removed, so a link
change still sends one ROUTE_UPDATE per origin, and the Rendezvous Hash and
per-broadcast choices among neighbours are unaffected: a node keeps one
entry per origin per neighbour as today. The echo and the drain rule stay
exactly as they are. Rejected: the deferred echo above; never dropping the
distance, about 16 bytes per publisher ever seen, which a peer can grow on
purpose; a timer as in RFC 8966, whose safety rests on stale messages being
gone by then; and making only relays origin nodes, rejected again.

What the model showed, all in
`rs/moq-net/verify/route/DRAIN.md` on the branch
`route-drain-counterexamples` (`5e7e3eb434`), which also holds the model
with all three variants behind switches, per-link costs, the configs that
reproduce each result, and four `Sim` replays of the original count:

- With the list, the triangle passes at full bounds, and the origin behind
  a triangle passes with two flaps at unit cost and at four unequal cost
  sets. The same holds with the echo free to overtake its sender's other
  messages, which is today's wire; without the list that ordering counts to
  infinity in 11 states.
- The echo cannot go: with the list and no echo, a node forgets while a
  neighbour still routes through its advertisement, and `LoopFree` fails in
  944 states.
- A self-naming route is kept as an unusable entry until it ends. Dropping
  it outright lets collection run while the neighbour's route is still open.
- Not run: full bounds on the 4-relay shapes, any 5-relay shape, and the
  runs listed there as stopped. No message count was measured.

So this lands on today's Announce Streams and does not wait for
[the route stream](/quest/m2/cluster-routing/routes/route-stream.md), which
later puts the echo in order by construction. A recommendation the
maintainer has not confirmed; say so in the PR.

Work, model first:

- In `RouteHandoff.tla`, keep the echo on its own queue as on `main`, add
  the list and a cost per link, add the origin behind a triangle (unit and
  unequal costs) to the checked configs, and turn `NoCountToInfinity` on in
  every config. Delete `CountToInfinity.cfg` as an open finding. If
  [moq-dev/moq#8479](https://github.com/moq-dev/moq/pull/8479) has landed,
  that includes its line, ring, and chord, with costs; if not, whichever
  lands second adapts.
- **The list's one cost is unmeasured, so measure it before the router.**
  An upstream path change at equal metric now sends a ROUTE_UPDATE to
  everyone downstream, where today it stops. Run the list against #8479's
  announce budget (4 routing messages per session direction per external
  change) on its meshes. If it breaks the budget or brings hunting back,
  stop and report it rather than raising the budget.
- Say what stays unexplored. Run TLC one config at a time on the recipe's
  settings; the 4-relay shapes with the origin inside a cycle did not finish
  in 40 minutes on a loaded host.
- Router: an advertisement and a route entry carry the list; `route` marks
  an entry that names this node; `selected`, the infeasible set, and the
  certifiers skip it, so it never holds a route or asks for a seqno; a node
  appends its own Node ID when it advertises. Turn the four replays and the
  two-flap trace into tests that the metric never passes the longest
  loop-free path.
- Wire, for the implementer to confirm or improve; list each departure in
  the PR. ROUTE_START and ROUTE_UPDATE gain a count and that many Node IDs,
  in order from the origin, the sender last and the origin itself left out.
  A node checks the list for its own Node ID as received, before it files
  anything a scoped peer sent under a qualified Node ID. A list longer than
  a fixed limit makes the route unusable; reuse lite-06's hop limit unless
  there is a reason not to. A node that forwards declares a Node ID, since
  it has to name itself.
- Draft: the two messages, Routing (a route that names the receiver, and
  that forgetting is safe because of it, not because of the echo alone),
  and the lite-07 changelog. `js/net` has no route layer yet, so
  [its quest](/quest/m2/cluster-routing/routes/js.md) decodes the list and
  sends an empty one for its own route.
- Update the `verify-route` skill's open finding, coverage, and sync
  commit, and the epic's loop-freedom bullet and decision 6, which say the
  echo alone makes forgetting safe.
- [moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457)'s cache
  sweep runs on the Rust router and depends on the feasibility check; rerun
  it with the list if it has landed.
- Run `just check`, `just test interop --all`, and `just drafts check`.

#8487 holds this quest's branch with the echo moved behind the echoer's
routes in the model and `Sim`. That change is no longer needed here and
belongs to the route stream; start again from `main`'s model.

Rollout: lite-07 is `-wip`, so relays pinned to `main` roll together, as
each earlier lite-07 wire change did.

Caused by: [moq-dev/moq#7676](https://github.com/moq-dev/moq/pull/7676),
whose drain rule assumed a per-neighbour echo proves no derived route
remains, and [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023),
which put the echo on the answering stream.

Public API: none. Wire: lite-07-wip, a hop list on ROUTE_START and
ROUTE_UPDATE.

## Related

- [Each side says everything about routing on one stream](/quest/m2/cluster-routing/routes/route-stream.md) - puts the echo in order later; this does not wait for it
- [js/net route layer](/quest/m2/cluster-routing/routes/js.md) - decodes the list in JS
