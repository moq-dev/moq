---
name: verify-route
description: Model-check the lite-07 route rule (`rs/moq-net/verify/route/RouteHandoff.tla`, the rule the routes epic's Plan item 27 picked, as `route::path::Router` in `rs/moq-net/src/route/path.rs` runs it) with TLA+. Use when a change touches route paths, the DEAD message for a closed session, holds, the doubt and its timer, forgetting dead hops, routes per prefix and scoped streams, the down-only flag on upstream links, the newest-floor views, what a reader is shown, or where a request goes, or when a routing bug needs every message ordering checked rather than a random sweep.
---

# Verify the route rule

`rs/moq-net/verify/route/RouteHandoff.tla` models one publisher's routes across a mesh, under the rule the routes epic's Plan item 27 picked: a route names every relay it crossed with the session it learned it on, a closed session is named in one DEAD message per neighbour, a relay with no route holds until those that learned it from the relay answer, and only along the path it was built on, so holds never close a cycle, and a closed session casts doubt on its far end until a timer fires. A relay ranks routes by drain, then the lowest link priority on the path, then cost; an offer carries its drain, so no relay ranks a path above what its neighbour did. A relay that hears GOAWAY sends a DRAIN naming the leaving node, the DEAD message's other entry, to each neighbour it sent a route through that node, and each relay doubts that node, so none takes a stale offer through it. A draining path ranks worst but stays usable. A route learned on a link its learner marks upstream, or from a down-only route, is down-only, and no relay sends one over a link it marks upstream (`Upstream`, the links whose higher-numbered end marks them), so no relay carries traffic between two upstreams. It follows `route::path::Router` in `rs/moq-net/src/route/path.rs` with one publisher and a route per prefix (`"root"`, and `"a"` inside it, which relays in `Scoped` start out watching alone), and keeps dead hops until their timer fires. A relay asks each neighbour for the set of prefixes it watches, none inside another; a relay in `Editors` may change that set up to `MaxEdits` times. Each change is an edit, sent as an ask on the FIFO and answered by a marker on the neighbour's FIFO, where both sides rekey the routes between them alike (`Rekey`): a key the new set covers stays, the route at a removed `"a"` moves to an added `"root"` without its floor, and the route at a removed `"root"` carries its floor, presented at `"a"`, to an added `"a"`. A heard route is believed for a prefix only while the relay asked for it at the last marker and in every edit still in flight. Every entry point is one atomic step ending in `Update`; messages share one FIFO per direction. The node timer is `Lift`; each dead hop also expires independently through `Expire`, so repeated failures of one node cannot retain all its old hops. Either may fire at any moment, or with `Patient` only once no relevant DEAD is in flight. Every lite-07 session drives the path router through `route::node::Node` since [the switch](https://github.com/moq-dev/moq/pull/8731), and `route::tests::Sim` drives it under `Rule::PATH` (timer-only doubt).

## Rules

Each rule is an INVARIANT; the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `Truthful` | Every view a relay keeps, and every floor it was offered, is the publisher's claims at that revision within its prefix. | A viewer sees a set of broadcasts the publisher never had. |
| `Mirrored` | With nothing in flight from one relay to another, what the first sent is what the second heard: the same routes, on the same paths, held exactly where the second holds them. | An edit rekeys the two sides differently, and they disagree about a route for good. |
| `Seen` | A relay shows a claim exactly while it holds a route covering it and the newest floor a believed covering route gave it has the claim, as that floor says. | A reader sees a claim no route vouches for, or an older one than it learned. |
| `NoRevival` | A reader never sees START for an item it saw END for, unless it forgot it since. | An ended broadcast comes back. |
| `NoFalseEnd` | A reader sees END only once the publisher withdrew that item. | A live broadcast is reported as ended rather than offline. |
| `NoRegression` | A reader never sees an UPDATE to an older item. | A stale announcement replaces a newer one. |
| `Stable` | A step that only changes a route's path, lifts a doubt, or applies an interest edit (its ask, or its marker for the claims both of the edit's sets cover) changes a shown claim only to a newer floor, or lets go of one it held unavailable. | A detour flaps claims, as a seqno advance did under the distance, or an edit that swaps prefixes ends or re-sends a claim both sets cover. |
| `SimplePaths` | Every route held or in flight names a loop-free path from the publisher to its sender. | A relay took back a route that crossed it. |
| `DownOnly` | A route is down-only exactly when its path came down a link its learner marks upstream, no path climbs a link its sender marks upstream after that, and nothing down-only is sent, in flight, or heard over such a link. | A relay carries CDN traffic between two upstreams, or a route loses its down-only flag one LAN hop on. |
| `LoopsAreStale` | A request that loops crosses a relay whose route names a closed session. | Routing loops with every relay up to date. |
| `StaleIsChased` | Every stale route a relay may use has the news that ends it on its way: queued toward the relay, or toward the neighbour it came from, and so on back. | A stale route, and any loop through it, lasts for good. |
| `NoStaleReturn` | No relay may use a route through a hop it once learned had died. | A dead path comes back after a relay forgot it. |
| `QuietIsSettled` | Once nothing is in flight and every timer fired: each relay shows exactly the claims a path of open sessions through relays watching them carries to it, never up a link its sender marks upstream after one its learner does, available, and a request for each reaches the publisher; no relay holds, keeps a stale route, or remembers a dead hop; and both ends of each session agree on what it asks for. | A hold never ends, a claim is shown that no path carries, a relay behind a scoped neighbour never reaches what a farther one offers, or an edit never takes effect. |
| `Budget` | From one quiet point to the next, the messages sent stay within 4 each way over each configured session per prefix and per claim, for each external change. | One change sets off announce spam, or routing never goes quiet. |
| `RequestsLoopFree` | A request never loops. | A request cycles through relays instead of reaching the publisher. |

Every config checks `RequestsLoopFree`; `Loop.cfg` pins the three-failure regression.

No config checks a liveness property: `Spec` has no fairness, so TLC does not show that a hold ends. That one does is an argument over what TLC does check. `Budget` bounds the messages a burst sends, each delivery takes one message off a queue, and a doubt starts only at a close or a delivery, so a burst is a finite number of steps. A delivery is enabled while a queue holds a message and a timer may fire once no news about its node is in flight, so a burst left alone reaches the state with nothing in flight and no doubt. `QuietIsSettled` says no relay holds there.

What the timer costs, as the configs show it: fired late, it costs only the wait, and every rule holds (`RouteHandoff.cfg`). Fired early, a stale route can return, as with no doubt at all, and nothing else breaks (`Early.cfg`, which passes without `NoStaleReturn` and fails it within seconds with it). With two failures a stale route can return whatever the timer (`Twice.cfg` drops `NoStaleReturn`), and `StaleIsChased` is the bound.

## Run

```sh
quest gate just rs tla moq-net/verify/route/RouteHandoff.tla
quest gate just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/Ring.cfg
```

Choose the config affected by the change; do not run the directory locally.
Nightly runs every config through `just test tla`, simulating each with a
`Simulate:` line. The recipe uses four workers. `Loop.cfg` checks the timer-only
doubt regression. When calling `just rs tla`
directly, pass the arguments from that line explicitly. For covering-source
selection, for example:

```sh
quest gate just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/Overlap.cfg -simulate num=1000 -depth 500
```

The older exhaustive counts and times below predate independent hop expiry
(`Expire`); they are historical baselines, not validation of the current model.
The Loop row gives its current simulation parameters. Times used four workers.
`Patient` is TRUE unless the table says otherwise.

`Loop.cfg` used to stop at the first early-collection counterexample. With
timer-only doubt, the attempted exhaustive check was stopped by the user after
76 minutes: 32.1M distinct states, 19.3M still queued at depth 14, and a growing
queue. It did not complete. Routine checks now use its bounded `Simulate:` line,
as they do for the other large configs. This samples the invariants; it is not
an exhaustive proof. Do not rerun the full route spec exhaustively for this
change; faster checkers are separate work.

| Config | Mesh | Bounds | Previous distinct states | Previous run |
| --- | --- | --- | --- | --- |
| `RouteHandoff.cfg` | The publisher behind relay 1, in a triangle with relays 2 and 3. | 1 claim change, 1 session change | 476,434 | a minute or two |
| `Early.cfg` | The same, with a timer that fires at any moment. | 1 claim change, 1 session change | 14,220,217 | half an hour |
| `Twice.cfg` | The same, where two sessions close, as when relay 1 dies. Simulated: `num` counts behaviours per worker. | 1 claim change, 2 session changes | 40,000 behaviours, 4.0M states | a few minutes |
| `Ring.cfg` | The publisher and three relays in a ring, with a timer that fires at any moment. | 1 claim change, 1 session change | 813,075 | a minute or two |
| `Lag.cfg` | Relay 3 hears the publisher through relays 1 and 2: the lagging neighbour. | 2 claim changes, 1 session change | 1,702,001 | a few minutes |
| `LagWide.cfg` | The same, with relay 1 asking for `"a"` alone. | 2 claim changes, 1 session change | 3,846,369 | a quarter of an hour |
| `Dead.cfg` | Two relays behind a relay whose only uplink asks for `"a"` alone, one with a dear session of its own to the publisher. Simulated. | 3 claim changes, 1 session change | 40,000 behaviours, 4.8M states | ten minutes |
| `Blind.cfg` | A relay whose nearest neighbour asks for `"a"` alone, and a farther one knows everything. Simulated. | 2 claim changes, 1 session change | 40,000 behaviours, 4.8M states | ten minutes |
| `Overlap.cfg` | Five relays, three asking for `"a"`, with overlapping cheap broad and dear narrow paths. Simulated past the message budget. | 1 claim change, no session change | 1,000 behaviours per worker (4,000 on four workers), depth 500 | simulation |
| `Loop.cfg` | The publisher with a session to each of three relays in a triangle, all closing. Samples `RequestsLoopFree` with timer-only doubt. | 3 session changes | 1,000 behaviours per worker, depth 150, seed 1 | simulation |
| `Drain.cfg` | The publisher joined to two relays joined to each other; both hear its GOAWAY. Ranking only a relay's own drain exceeded `Budget`. | 1 claim change, 1 session change, GOAWAY on each session | 511,147 | two minutes |
| `DrainRing.cfg` | The publisher joined to three relays in a ring that is cheap one way round (`Ring`); all hear its GOAWAY. Without the DRAIN news and its doubt, a request looped round the ring with no session closed, breaking `LoopsAreStale`. Simulated. | 1 claim change, GOAWAY on each session | 1,000 behaviours per worker, depth 150, seed 1 | about a minute |
| `Prefer.cfg` | The same triangle, where both relays prefer the link between them. Ranking only a relay's own link priority exceeded `Budget`. | 1 claim change, 1 session change | 12,654 | seconds |
| `Upstream.cfg` | Two uplinks: the publisher on a CDN relay joined by a dear link to another, and a local mesh of two relays, each with an upstream link to one CDN relay. With the CDN link down, the second CDN relay has no route rather than one through the local mesh. The Rust fixture is `two_uplinks_never_carry_cdn_transit`. Without the export check, or with a flag that a LAN hop clears, `QuietIsSettled` and `DownOnly` fail within seconds. | 1 claim change, 1 session change | 68,717 | seconds |
| `UpstreamScoped.cfg` | The same, where the second CDN relay asks for `"a"` alone, so the stream to it over an upstream link picks among covering routes that are all down-only and ends. | 1 claim change, 1 session change | 69,576 | seconds |
| `EditNarrow.cfg` | The publisher with sessions to relays 1 and 2, the one to relay 2 dear; relay 2 watches everything and narrows to `"a"` or drops it once. | 1 claim change, 1 session change, 1 edit | 768,483 | three minutes |
| `EditWiden.cfg` | The same, where relay 2 watches `"a"` and widens to everything or drops it once. | 1 claim change, 1 session change, 1 edit | 3,655,573 | a quarter of an hour |
| `Edit.cfg` | The same with two edits, such as widening then narrowing back, or dropping everything then asking again. Before the rekey kept the route the sender would reflect, a widening then a narrowing crossing the close of relay 2's own session kept a route relay 1 learned through it and took a claim offline, breaking `Stable`. Simulated; an exhaustive pass covered every behaviour to depth 18 (10.6M states) before it was stopped. | 1 claim change, 1 session change, 2 edits | 40,000 behaviours, 4.8M states | four minutes |
| `HoldCycle.cfg` | The publisher on a hub with two open relays, and two relays asking for `"a"` alone, each joined to both open relays and nearer the other's choice. Before a hold followed only the path it was built on, the publisher's session closing left four holds waiting on each other for good, breaking `QuietIsSettled` within seconds. Simulated. | 1 claim change, 1 session change | 2,000 behaviours per worker, depth 200, seed 1 | a few minutes |

Run the affected config last, with at most one TLC process at a time. First run what answers in seconds: the router's tests (`quest gate just rs test -p moq-net --lib -E 'test(/route::tests::(handoff|residual|scope|compare)/)'`, with `MOQ_ROUTE_SCENARIOS` raising the sweeps' count), then TLC's simulation mode with raised bounds in a copy of a config, such as `-simulate num=3000 -depth 150`.

## Search the router

`rs/moq-net/src/route/tests/search.rs` searches every order in which `Sim` can deliver, disconnect, connect, collect, fire a timer, and change a claim, on the compiled `path::Router`, depth first with a fingerprint per state. It checks `Truthful` (`Sim::check`), `NoRevival`, `NoFalseEnd`, `NoRegression`, `Stable`, `SimplePaths` (`Sim`'s own assertions on each path sent), `RequestsLoopFree`, `QuietIsSettled`, and `Budget` in every state, and prints a broken rule's path as `Sim` calls. It leaves out `Seen`, `StaleIsChased`, and `NoStaleReturn`, which read state the model names and the router keeps privately. Its timers are the router's own one-second timers, so a doubt raised earlier fires earlier, where the model's `Lift` fires in any order. The gate searches the triangle under `RouteHandoff.cfg`'s bounds; Nightly's `just test route-sweep` searches `RouteHandoff.cfg`'s own mesh, a dear link, nested claims, a scoped relay, a scoped grant, and a six-node mesh.

```sh
quest gate just rs test -p moq-net --release --run-ignored all -E 'test(/^route::tests::search::/)'
```

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the router with `Sim::with(Rule::PATH, nodes)` (`deliver`, `disconnect`, `connect`, `announce`, `wait`), one call per step, except a timer. `Lift(n, x)` fires one timer on one relay, where `wait(HOP)` moves every relay's clock by one hop of delay: call it until that doubt lifts, and where another doubt lifted with it, the replay took a different path, which is no defect in the model.

## Coverage

Sync commit: `857f2eea4ac4cc7a3fb821f7d250637eb464b248`, where [the switch](https://github.com/moq-dev/moq/pull/8731) took in [the interest set](https://github.com/moq-dev/moq/pull/8728). Run `git log 857f2eea4.. -- rs/moq-net/src/route/path.rs rs/moq-net/src/route/reader.rs` to see what changed since. Prefix and reader indexes make the router's updates incremental; test-only full recomputation checks their result against the complete view. The model recomputes every prefix atomically and leaves that optimization out. `Rank` orders routes as `Offer::rank` does: drain, carried with each offer or known from a DRAIN (`Drains`), then the lowest link priority on the path (`Preferred`), then cost; `GoAway` marks a neighbour's session draining. `SelectKey` sets a route's down-only flag as `Router::usable_offer` does, and `Want0` ends a live down-only route toward an upstream link. `SrcKey` mirrors the router's preference for the best-ranked covering live path that does not name its receiver, leaving out down-only routes toward an upstream link and falling back to a held one, with the narrower scope breaking ties; `Select` plans live routes and their reflected sources before deciding holds; a held route keeps its last live path, so `SelectKey` holds under a next hop only on the path it was built on and `Want` ends a held stream whose source now has another path; `Advertise1` forwards a newer floor even when the claims did not change. The router keeps doubt until its one-second timer fires (`Lift::Timer`, `Doubt::DEFAULT`); the model's `Collect` leaves it for `Lift`. The comparison-only `Rule::EARLY` lifts doubt once no heard route names its node and is outside this timer-only model. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/route/path.rs` `Router::run`, `Router::select`, `Router::retain` | `Select` plans live routes and their reflected sources before `SelectKey` decides holds. |
| `rs/moq-net/src/route/path.rs` `Offer::rank`, `Router::goaway`, `Router::drains`, `Router::report_draining`, `Router::dead` (`Dead::Draining`) | `Rank`, `Below`, `PathPrio`, `CostAt`, `GoAway`, `MarkDrain`, `Credited`, `ReportDrain`, `Recv` |
| `rs/moq-net/src/route/path.rs` `Router::pending`, `Router::hopes`, `Router::doubts_path` | `Pending`, `Hopes`, `Doubted` |
| `rs/moq-net/src/route/path.rs` `Router::src` | `SrcKey` |
| `rs/moq-net/src/route/path.rs` `Router::usable_offer`, `Offer` | `SelectKey`'s `Down`, `Want0`'s export check, and `DownOnly`: the down-only flag is set on a route learned over an upstream link or from a down-only one, and never sent over an upstream link. |
| `rs/moq-net/src/route/path.rs` `Router::want`, `Router::stream`, `Router::advertise`, `Router::report` | `Want`, `Want0`, `Streams`, `Advertise`, `Report` |
| `rs/moq-net/src/route/path.rs` `Router::route`, `Router::batch`, `Router::dead`, `Router::fail`, `Router::disconnect`, `Router::connect`, `Router::change` | `Recv`, `Fail`, `Disconnect`, `Connect`, `ChangeClaim` |
| `rs/moq-net/src/route/path.rs` `Router::collect_doubt`, `Router::timeout` | `Collect`, `Lift`, `Expire`: each dead hop's own timer retires it independently of later failures of the same node; reporting an already retained hop again does not restart its expiry. |
| `rs/moq-net/src/route/path.rs` `Router::know`, `Router::believed`, `Router::watched`, `Router::shown_now`, `Router::show` | `Know`, `NewView`, `Believed`, `Watched`, `ShownNow` |
| `rs/moq-net/src/route/path.rs` `Router::watch`, `Router::ask`, `Router::asks`, `Router::edit`, `Router::edited`, `Session::asking`, `rekey` | `Watch`, `Asks`, `Answer`, `Marker`, `Asking`, `Rekey`, `Moved`: the model's relays watch with no scope regions, so their asks are what they watch. |
| `rs/moq-net/src/route/reader.rs` `Reader::apply` | `Events` |
| `rs/moq-net/src/route/tests/mod.rs` `Sim::deliver` | `Deliver` |

## Not covered

SYNCED is left out: the router only records it per prefix (`Router::synced`, `Router::is_synced`), and nothing it decides reads it. So are scope regions: a relay asks for what it watches, and `interest.rs`'s `a_scope_with_several_regions_asks_for_each` and the sweeps cover a scope met with the interest.

One publisher, so routes for several publishers are left out, and so are the copies and copy rank of cache-aware routing, which `rs/moq-net/src/route/tests/cache.rs` sweeps; the DEAD message's per-publisher batching reduces to one route per prefix. Two prefixes, one nested in the other, so an edit adds, removes, widens, or narrows one prefix at a time: no wildcards, swaps of several prefixes for one, stream cancellation, grants, qualified keys, or hidden claims. Leaves out two sessions between one pair of nodes at once, as a GOAWAY migration has: the model keeps one per pair, and among equal routes the router stays on the next hop's node and takes its latest session there (`a_later_session_to_the_next_hop_takes_over_among_equals`). Leaves out restarts, priorities other than 0 and 1, a priority that differs between a link's two ends, costs other than 1 and 2, RTT, and the hop limit. Down-only routes appear only where every relay's best route is also its only exportable one, so the model never shows a down-only best route hiding an exportable alternative, a cost the routes epic's contract accepts, and a relay that accepted a request from an upstream link never ends it when its route turns down-only. The hold deadline (`HOLD`), the stalled-writer close, and a scoped session's DEAD (`fail_heard`, which doubts and remembers nothing) are left out: a hold that ends early only lets go sooner, and the model's sessions are all unscoped. So are a session's mount keys: the node files each lite-07 session as one router key for both directions (`route/node.rs`), the model's one neighbour per session, edited in place, and a handle that reads through a mount answers under a further key of that session, which only sends. Messages come only from honest routers: validation of an unrelated DEAD or its far node, and empty or overlong replacement paths, is outside the model's input domain. Rust fixtures cover those guards and exportable covering-source selection, including keeping a covering hold when every live source is unexportable. Interest-edit fixtures also cover a cancelled stream retaining its pending child replies; the model has no stream cancellation. A claims message carries the sender's whole view for its prefix, equal to applying its changes on a FIFO stream. A route is sent on every stream whether or not the publisher has a claim there. The publisher ignores what it hears. `Overlap.cfg` also simulates five relays with every session up. No exhaustive pass has more than four relays or two session changes; `Sim`'s sweeps on the path rule (`sweep_handoff`, `sweep_scoped_handoff`, `sweep_scoped_meshes_show_what_the_rule_says`, and `compare.rs`'s churn) cover larger meshes, grants, qualified keys, interest edits, restarts, and churn.
