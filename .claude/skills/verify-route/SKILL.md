---
name: verify-route
description: Model-check the lite-07 route layer (`rs/moq-net/src/route/`) with TLA+. Use when a change touches route selection, feasibility, holds, seqno requests, ROUTE_END echoes, collection, claim floors and merging, or what a node's reader is shown, or when a routing bug needs every message ordering checked rather than a random sweep.
---

# Verify the route layer

`rs/moq-net/verify/route/RouteHandoff.tla` models one origin identity in `route::Router` as `route::tests::Sim` drives it: every router entry point is one atomic step ending in `update`, ROUTE, batches, and ROUTE_REQUEST share one FIFO per direction, and echoes travel on their own queue. A config picks the mesh: which nodes there are, which sessions join them, and which are clients (`Leaves`, as `Sim::leaf`) that forward no route but echo every ROUTE_END. TLC checks every ordering of those messages under each config's bounds, or samples them at random for a mesh too large to exhaust.

One identity is enough for meshes with several origins: the router keys every piece of state by identity and recomputes one identity from that identity's state alone, so each identity's messages, read on their own, are a run of this model with that origin. Sharing one FIFO per direction only removes orderings, and a session flap is a flap for each. The configs put the origin at the end of a line, in a ring, and on a client off a ring instead.

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
| `Budget` | From one quiet point to the next, the ROUTE, batch, ROUTE_REQUEST, and echo messages sent stay within 4 per direction of each configured session for each external change made (a claim change, a session opening or closing, a collection). | One change sets off announce spam. |
| `NoCountToInfinity` | No relay selects a route longer than a loop-free path. | Stale routes bounce between relays with no bound. |

`Converges` is a PROPERTY: under fairness that something in flight is eventually delivered, routing goes quiet after the last external change and stays settled. `Budget` implies it, since the message count is part of the state: a cycle of deliveries that never goes quiet would grow the count past any budget. So every config checks `Budget`, and only `Line.cfg`, small enough for TLC's liveness checker, checks `Converges` directly.

The budget is about one message of each kind per directed session. The most one change costs in the triangle is 20 of the 24 it allows: withdrawing the last claim sends a batch, a ROUTE_END, and an echo across each of the six directions, plus two seqno requests that chase routes whose ends are still in flight. `route-mesh-sweep` holds the implementation to the same budget, counted from the Sim's `route_msgs + claim_msgs + requests`.

The constraint `Bounded` stops exploring one hop past `NoCountToInfinity`, so a count to infinity shows up there instead of running TLC forever.

## Run

```sh
just rs tla moq-net/verify/route/RouteHandoff.tla
just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/Line.cfg
just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/Ring.cfg
just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/Chord.cfg -simulate num=1000 -depth 600
just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/CountToInfinity.cfg
```

Each but the last checks every rule but `NoCountToInfinity` and passes at the sync commit. Times were measured with every core of a loaded 24-core host, before the recipe dropped to 8 workers.

| Config | Mesh | Bounds | Run |
| --- | --- | --- | --- |
| `RouteHandoff.cfg` | The origin and two relays in a triangle. | 2 revisions, 2 flaps, 1 collection | 24M states, 22 min |
| `Line.cfg` | The origin, three relays, and a watching client in a line; also checks `Converges`. | 2 revisions, 1 flap, 1 collection | 2M states, 7 min |
| `Ring.cfg` | The origin and three relays in a ring. | 2 revisions, 1 flap | 24M states, 34 min |
| `Chord.cfg` | A publishing client on a ring of five relays with a chord, and two watching clients. Simulated: `num` counts behaviours per worker. | 2 claims, 4 revisions, 4 flaps, 2 collections | 24K behaviours (28M states) in 15 min |
| `CountToInfinity.cfg` | The triangle. | As `RouteHandoff.cfg` | Fails in 9 steps |

Larger exhaustive meshes do not fit: `Line.cfg` at two flaps passed 12M states in 55 minutes without finishing. TLC's `SYMMETRY` does not help, because selection breaks ties by neighbour key, so no two relays are interchangeable; only watching clients on the same relay are, and one per relay covers them. `Chord.cfg` samples the larger meshes instead; its flaps can cut the ring into a line.

Read a counterexample with `/statechart`, passing it the spec, a config, and `RouteHandoff.view.ts`, which labels each relay's route and each step as the message that arrives. It gives the shortest path to each broken rule in those words.

## Open finding: an overtaking echo counts to infinity

`CountToInfinity.cfg` fails. A node drops its feasibility distance once every neighbour echoed its ROUTE_END (`update` in `mod.rs`), on the reasoning that no advertisement derived from its own can then still arrive. But an echo travels apart from the echoer's own routes (the answering stream carries it, per `node.rs`), so it can overtake an advertisement that neighbour derived from this node's and sent before the end. The node then accepts that advertisement as a fresh, longer route, and two relays can count up without end while each shows the broadcast available. `residual_an_overtaking_echo_counts_to_infinity` in `route/tests/routing.rs` replays it on the router. Once it is fixed, flip that test, move `NoCountToInfinity` into `RouteHandoff.cfg`, and delete `CountToInfinity.cfg`.

## Reading a failure

A counterexample is a path through the model, not yet a bug. Replay it on the router with `Sim` (`hold`, `deliver`, `deliver_ack`, `disconnect`, `collect`), one call per step. If the router does not follow the path, the model is wrong: fix the model to match the code. If it does, report the path and the rule in the code's words. Do not weaken a rule, add a constraint, or shrink the bounds until the check passes.

## Coverage

Sync commit: `529fa6ac9138c6f7b09ce8380fdb2407334bd6ca`. Run `git log 529fa6ac91.. -- rs/moq-net/src/route/` to see what changed since. After a change to a covered function, mirror it in the model, rerun both configs, and move the sync commit forward only once the model describes the code at the new commit.

| Code (`rs/moq-net/src/route/`) | Model |
| --- | --- |
| `mod.rs` `feasible`, `selected`, `infeasible`, `recoverable`, `live`, `certifiers` | `Feasible`, `Selected`, `InfeasibleSet`, `Recoverable`, `Live`, `Certifiers` |
| `mod.rs` `certified`, `prune`, `adopted` | `Certified`, `Prune`, `Adopted` |
| `mod.rs` `update`, `record`, `advertise`, `requests` | `Update`, `Record`, `Plan`, `Requests` |
| `mod.rs` `show`, `shown_now` | `Events`, `ShownNow` |
| `mod.rs` `route`, `batch`, `request`, `echo` | `OnRoute`, `OnBatch`, `OnRequest`, `Deliver`, `DeliverAck` |
| `mod.rs` `connect`, `peer_interest`, `disconnect`, `collect`, `change` | `Connect`, `Opened`, `Disconnect`, `Closed`, `Collect`, `ChangeClaim` |
| `view.rs` `View::merge`, `View::restrict` | `Merge`, `Certify` |
| `tests/mod.rs` `Sim` queues and `revivals`, `false_ends` | `q`, `acks`, `revivals`, `falseEnds` |
| `tests/mod.rs` `Sim::leaf`, `Stats` | `Leaves`, `burst` |

## Not covered

The model fixes every interest at the root, so it leaves out wildcards, `present`/`represent`/`retire`, interest changes, and the hidden `current` regions; prune's region bookkeeping reduces to "no certifier has a floor". It also leaves out scoped grants and qualified keys, GOAWAY, link priority, costs other than 1, upstream links, identity rotation and restart, `node.rs`'s per-stream keys, and how the origin above the router ranks two identities claiming the same broadcast. A batch carries the sender's whole presentation, equal to applying its diff on a FIFO stream. The origin drops the ROUTEs and batches it hears for its own identity, which change nothing there; `Budget` still counts them. An exhaustive pass covers one claim and only the bounds in the table above. `sweep_handoff` and `sweep_scoped_handoff` still own the rest.
