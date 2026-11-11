---
name: verify-route
description: Model-check the lite-07 route layer (`rs/moq-net/src/route/`) with TLA+. Use when a change touches route selection, feasibility, holds, seqno requests, ROUTE_END echoes, collection, claim floors and merging, or what a node's reader is shown, or when a routing bug needs every message ordering checked rather than a random sweep.
---

# Verify the route layer

`rs/moq-net/verify/route/RouteHandoff.tla` models one origin identity in `route::Router` as `route::tests::Sim` drives it: every router entry point is one atomic step ending in `update`, ROUTE, batches, and ROUTE_REQUEST share one FIFO per direction, and echoes travel on their own queue, free to overtake. A config picks the mesh: which nodes there are, which sessions join them, which cost 2, and which nodes are clients (`Leaves`, as `Sim::leaf`) that forward no route but echo every ROUTE_END. TLC checks every ordering of those messages under each config's bounds, or samples them at random for a mesh too large to exhaust.

One identity is enough for meshes with several origins: the router keys every piece of state by identity and recomputes one identity from that identity's state alone, so each identity's messages, read on their own, are a run of this model with that origin. Sharing one FIFO per direction only removes orderings, and a session flap is a flap for each. The configs put the origin at the end of a line, in a ring, behind a triangle, and on a client off a ring instead.

## Rules

Each rule is an INVARIANT; the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `Truthful` | Every view a relay keeps is the origin's claims at that view's revision. | A viewer sees a set of broadcasts the origin never had. |
| `AllCertified` | Every view and batch with claims has a floor. | The model's dropping of `loose` claims is unsound; widen the model first. |
| `NoRevival` | A reader never sees START for an item it saw END for, unless it forgot it since. | An ended broadcast comes back. |
| `NoFalseEnd` | A reader sees END only once the origin withdrew that item. | A live broadcast is reported as ended rather than offline. |
| `NoRegression` | A reader never sees an UPDATE to an older item. | A stale announcement replaces a newer one. |
| `LoopFree` | Selected next hops never cycle. | Requests loop between relays. |
| `QuietIsSettled` | Once nothing is in flight, each relay reaching the origin shows its claims, available, and routes only while it has any; each cut-off relay shows nothing and keeps no route, distance, or request. | Routing never converges, or a dead origin stays shown. |
| `SimplePaths` | Every route held or in flight names a loop-free path from the origin to its sender, at that path's cost. | A node took back a route that crossed it, the first step of a count to infinity. |
| `NoCountToInfinity` | No relay selects a route longer than a loop-free path. | Stale routes bounce between relays with no bound. |
| `Budget` | From one quiet point to the next, the ROUTE, batch, ROUTE_REQUEST, and echo messages sent stay within 4 each way over each configured session, per route identity and per claim, for each external change made (a claim change, a session opening or closing, a collection). | One change sets off announce spam. |

`SimplePaths` needs no search to believe: a node sends a route only after adding itself to hops that did not name it. It holds whatever a node forgot, which is why `NoCountToInfinity` no longer rests on the echo.

`Budget` is the one `mesh_sweep.rs` holds the implementation to, with the model's one identity. `Converges` is a PROPERTY: under fairness that something in flight is eventually delivered, routing goes quiet after the last external change and stays settled. `Budget` implies it, since the message count is part of the state: a cycle of deliveries that never goes quiet would grow the count past any budget. So every config checks `Budget`, and only `Line.cfg`, small enough for TLC's liveness checker, checks `Converges` directly.

## Run

```sh
just rs tla moq-net/verify/route/RouteHandoff.tla
just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/Behind.cfg
just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/BehindDear.cfg
just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/Line.cfg
just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/Ring.cfg
just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/Chord.cfg -simulate num=1000 -depth 600
```

Times are TLC 1.8.0 on Java 21 with `-workers 4` at `nice -n 19`, on a 24-core host.

| Config | Mesh | Bounds | Distinct states | Run |
| --- | --- | --- | --- | --- |
| `RouteHandoff.cfg` | The origin and two relays in a triangle. | 2 claim changes, 2 session changes, 1 collection | 24,192,399 | 39 min |
| `Behind.cfg` | The origin behind relay 1, which sits in a triangle with relays 2 and 3. | 1 claim change, 1 session change | BEHIND_STATES | BEHIND_TIME |
| `BehindDear.cfg` | The same, with relay 3 nearer the origin than relay 2. | 1 claim change, 1 session change | DEAR_STATES | DEAR_TIME |
| `Line.cfg` | The origin, three relays, and a watching client in a line; also checks `Converges`. | 2 claim changes, 1 session change, 1 collection | 1,543,073 | 13 min |
| `Ring.cfg` | The origin and three relays in a ring. | 2 claim changes, 1 session change | RING_STATES | RING_TIME |
| `Chord.cfg` | A publishing client on a ring of five relays with a chord, and two watching clients. Simulated: `num` counts behaviours per worker. | 2 claims, 4 claim changes, 4 session changes, 2 collections | 4,000 behaviours, 4.6M states | 9 min |

All pass at the sync commit. Run them last, one at a time. First run what answers in seconds:

- `just rs test -p moq-net --lib -E 'test(/route::/)'`. `sweep_no_route_outgrows_a_loop_free_path` delivers every message and echo in a random order on random meshes with unequal costs, flaps, restarts, and collections, and checks the metric bound, loop freedom, and the quiet state. Raise its case count for a wider search: 40,000 cases run in 15 seconds in release.
- TLC's simulation mode on the config in question, which samples deep runs the exhaustive bounds do not reach: add `-simulate num=3000 -depth 80`.

Both behind-a-triangle configs fail `SimplePaths` within seconds if `Crossed` is made false, which is the count to infinity the hops fix: a relay drops its distance once its neighbours echoed, and takes back a copy of its own route that a third relay still held.

Read a counterexample with `/statechart`, passing it the spec, a config, and `RouteHandoff.view.ts`, which labels each relay's route and each step as the message that arrives. It gives the shortest path to each broken rule in those words. The view labels any mesh and assumes equal costs.

## Reading a failure

A counterexample is a path through the model, not yet a bug. Replay it on the router with `Sim` (`hold`, `deliver`, `deliver_ack`, `disconnect`, `collect`), one call per step. If the router does not follow the path, the model is wrong: fix the model to match the code. If it does, report the path and the rule in the code's words. Do not weaken a rule, add a constraint, or shrink the bounds until the check passes.

## Coverage

Sync commit: `6ec81044992d88620122994111193b4ad07e74e3`. Run `git log 6ec8104499.. -- rs/moq-net/src/route/` to see what changed since. After a change to a covered function, mirror it in the model, rerun every config, and move the sync commit forward only once the model describes the code at the new commit.

| Code (`rs/moq-net/src/route/`) | Model |
| --- | --- |
| `mod.rs` `feasible`, `Entry::crossed`, `eligible`, `metric`, `selected`, `infeasible`, `recoverable`, `live`, `certifiers` | `Feasible`, `Crossed`, `Eligible`, `Through`, `Selected`, `InfeasibleSet`, `Recoverable`, `Live`, `Certifiers` |
| `mod.rs` `certified`, `prune`, `adopted` | `Certified`, `Prune`, `Adopted` |
| `mod.rs` `update`, `recompute`, `record`, `advertise`, `requests` | `Update` and its `prune`, `Record`, `Plan`, `Requests` |
| `mod.rs` `show`, `shown_now` | `Events`, `ShownNow` |
| `mod.rs` `route`, `batch`, `request`, `echo` | `OnRoute`, `OnBatch`, `OnRequest`, `Deliver`, `DeliverAck` |
| `mod.rs` `connect`, `peer_interest`, `disconnect`, `collect`, `change` | `Connect`, `Opened`, `Disconnect`, `Closed`, `Collect`, `ChangeClaim` |
| `view.rs` `View::merge`, `View::restrict` | `Merge`, `Certify` |
| `tests/mod.rs` `Sim` queues and `revivals`, `false_ends` | `q`, `acks`, `revivals`, `falseEnds` |
| `tests/mod.rs` `Sim::leaf`, `Stats` | `Leaves`, `burst` |

## Not covered

The model fixes every interest at the root, so it leaves out wildcards, `present`/`represent`/`retire`, interest changes, and the hidden `current` regions; prune's region bookkeeping reduces to "no certifier has a floor". It also leaves out scoped grants and qualified keys (so `advertised`, which only they read), GOAWAY, link priority, costs other than 1 and 2, the hop limit, the informational RTT, upstream links, identity rotation and restart, `node.rs`'s per-stream keys, and how the origin above the router ranks two identities claiming the same broadcast. A batch carries the sender's whole presentation, equal to applying its diff on a FIFO stream. The origin drops the ROUTEs and batches it hears for its own identity, which change nothing there; `Budget` still counts them. An exhaustive pass covers one claim at the bounds in the table above, and no mesh of more than four relays. `sweep_no_route_outgrows_a_loop_free_path`, `sweep_handoff`, `sweep_scoped_handoff`, and `mesh_sweep.rs` still own the rest: larger meshes, more churn, restarts, and scoped grants.
