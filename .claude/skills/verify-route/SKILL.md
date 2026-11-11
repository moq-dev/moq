---
name: verify-route
description: Model-check the lite-07 route layer (`rs/moq-net/src/route/`) with TLA+. Use when a change touches route selection, feasibility, holds, seqno requests, ROUTE_END echoes, collection, claim floors and merging, or what a node's reader is shown, or when a routing bug needs every message ordering checked rather than a random sweep.
---

# Verify the route layer

`rs/moq-net/verify/route/RouteHandoff.tla` models one origin identity in `route::Router` as `route::tests::Sim` drives it: every router entry point is one atomic step ending in `update`, ROUTE, batches, and ROUTE_REQUEST share one FIFO per direction, and echoes travel on their own queue. TLC checks every ordering of those messages in a triangle of the origin and two relays, under the bounds in the config.

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
| `NoCountToInfinity` | No relay selects a route longer than a loop-free path. | Stale routes bounce between relays with no bound. |

The constraint `Bounded` stops exploring one hop past `NoCountToInfinity`, so a count to infinity shows up there instead of running TLC forever.

## Run

```sh
just rs tla moq-net/verify/route/RouteHandoff.tla
just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/CountToInfinity.cfg
```

The first checks every rule but `NoCountToInfinity`: about 3.7M states, two minutes on 32 cores, and it passes at the sync commit. The second fails at the sync commit on the open finding below. `just test tla rs/moq-net/verify/route` runs every config here, as Nightly does, and fails if a config with an `\* Open finding:` line passes.

Read a counterexample with `/statechart`, passing it the spec, a config, and `RouteHandoff.view.ts`, which labels each relay's route and each step as the message that arrives. It gives the shortest path to each broken rule in those words.

## Open finding: an overtaking echo counts to infinity

`CountToInfinity.cfg` fails. A node drops its feasibility distance once every neighbour echoed its ROUTE_END (`update` in `mod.rs`), on the reasoning that no advertisement derived from its own can then still arrive. But an echo travels apart from the echoer's own routes (the answering stream carries it, per `node.rs`), so it can overtake an advertisement that neighbour derived from this node's and sent before the end. The node then accepts that advertisement as a fresh, longer route, and two relays can count up without end while each shows the broadcast available. `residual_an_overtaking_echo_counts_to_infinity` in `route/tests/routing.rs` replays it on the router. Once it is fixed, flip that test, move `NoCountToInfinity` into `RouteHandoff.cfg`, and delete `CountToInfinity.cfg`.

## Reading a failure

A counterexample is a path through the model, not yet a bug. Replay it on the router with `Sim` (`hold`, `deliver`, `deliver_ack`, `disconnect`, `collect`), one call per step. If the router does not follow the path, the model is wrong: fix the model to match the code. If it does, report the path and the rule in the code's words. Do not weaken a rule, add a constraint, or shrink the bounds until the check passes.

## Coverage

Sync commit: `103052ed074a87f00269ecb0d639905482ecfb6f`. Run `git log 103052ed07.. -- rs/moq-net/src/route/` to see what changed since. After a change to a covered function, mirror it in the model, rerun both configs, and move the sync commit forward only once the model describes the code at the new commit.

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

## Not covered

The model fixes every interest at the root, so it leaves out wildcards, `present`/`represent`/`retire`, interest changes, and the hidden `current` regions; prune's region bookkeeping reduces to "no certifier has a floor". It also leaves out scoped grants and qualified keys, GOAWAY, link priority, costs other than 1, upstream links, identity rotation and restart, and `node.rs`'s per-stream keys. A batch carries the sender's whole presentation, equal to applying its diff on a FIFO stream. The origin drops the ROUTEs and batches it hears for its own identity, which change nothing there. A pass covers one claim, two revisions, two session opens or closes, and one collection in that triangle. `sweep_handoff` and `sweep_scoped_handoff` still own the rest.
