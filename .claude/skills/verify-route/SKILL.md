---
name: verify-route
description: Model-check the lite-07 route rule (`rs/moq-net/verify/route/RouteHandoff.tla`, the rule the routes epic's Plan item 27 picked, as `rs/moq-net/src/route/tests/paths.rs` prototypes it) with TLA+. Use when a change touches route paths, the DEAD message for a closed session, holds, the doubt and its timer, forgetting dead hops, routes per prefix and scoped streams, the newest-floor views, what a reader is shown, or where a request goes, or when a routing bug needs every message ordering checked rather than a random sweep.
---

# Verify the route rule

`rs/moq-net/verify/route/RouteHandoff.tla` models one publisher's routes across a mesh, under the rule the routes epic's Plan item 27 picked: a route names every relay it crossed with the session it learned it on, a closed session is named in one DEAD message per neighbour, a relay with no route holds until those that learned it from the relay answer, and a closed session casts doubt on its far end until a timer fires. It follows the prototype `Relay` in `rs/moq-net/src/route/tests/paths.rs` under `Rules::waits`, adds a route per prefix (`"root"`, and `"a"` inside it, which relays in `Scoped` ask for alone), and keeps dead hops until their timer fires. Every entry point is one atomic step ending in `Update`; messages share one FIFO per direction. The timer is the step `Lift`, which may fire at any moment, or with `Patient` only once no DEAD about its node is in flight. `route::Router` still runs Babel's distance; [the router quest](/quest/m2/cluster-routes/scope.md) builds this rule.

## Rules

Each rule is an INVARIANT; the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `Truthful` | Every view a relay keeps, and every floor it was offered, is the publisher's claims at that revision within its prefix. | A viewer sees a set of broadcasts the publisher never had. |
| `Seen` | A relay shows a claim exactly while it holds a route covering it and the newest floor a believed covering route gave it has the claim, as that floor says. | A reader sees a claim no route vouches for, or an older one than it learned. |
| `NoRevival` | A reader never sees START for an item it saw END for, unless it forgot it since. | An ended broadcast comes back. |
| `NoFalseEnd` | A reader sees END only once the publisher withdrew that item. | A live broadcast is reported as ended rather than offline. |
| `NoRegression` | A reader never sees an UPDATE to an older item. | A stale announcement replaces a newer one. |
| `Stable` | A step that only changes a route's path or lifts a doubt changes a shown claim only to a newer floor, or lets go of one it held unavailable. | A detour flaps claims, as a seqno advance did under the distance. |
| `SimplePaths` | Every route held or in flight names a loop-free path from the publisher to its sender. | A relay took back a route that crossed it. |
| `LoopsAreStale` | A request that loops crosses a relay whose route names a closed session. | Routing loops with every relay up to date. |
| `StaleIsChased` | Every stale route a relay may use has the news that ends it on its way: queued toward the relay, or toward the neighbour it came from, and so on back. | A stale route, and any loop through it, lasts for good. |
| `NoStaleReturn` | No relay may use a route through a hop it once learned had died. | A dead path comes back after a relay forgot it. |
| `QuietIsSettled` | Once nothing is in flight and every timer fired: each relay shows exactly the claims a path of open sessions carries to it, available, and a request for each reaches the publisher; no relay holds, keeps a stale route, or remembers a dead hop. | A hold never ends, a claim is shown that no path carries, or a relay behind a scoped neighbour never reaches what a farther one offers. |
| `Budget` | From one quiet point to the next, the messages sent stay within 4 each way over each configured session per prefix and per claim, for each external change. | One change sets off announce spam, or routing never goes quiet. |
| `RequestsLoopFree` | A request never loops. | Only `Loop.cfg` checks it, as an open finding. |

No config checks a liveness property: `Spec` has no fairness, so TLC does not show that a hold ends. That one does is an argument over what TLC does check. `Budget` bounds the messages a burst sends, each delivery takes one message off a queue, and a doubt starts only at a close or a delivery, so a burst is a finite number of steps. A delivery is enabled while a queue holds a message and a timer may fire once no news about its node is in flight, so a burst left alone reaches the state with nothing in flight and no doubt. `QuietIsSettled` says no relay holds there.

What the timer costs, as the configs show it: fired late, it costs only the wait, and every rule holds (`RouteHandoff.cfg`). Fired early, a stale route can return, as with no doubt at all, and nothing else breaks (`Early.cfg`, which passes without `NoStaleReturn` and fails it within seconds with it). With two failures a stale route can return whatever the timer (`Twice.cfg` drops `NoStaleReturn`), and `StaleIsChased` is the bound.

## Run

```sh
quest gate just rs tla moq-net/verify/route/RouteHandoff.tla
quest gate just rs tla moq-net/verify/route/RouteHandoff.tla -config rs/moq-net/verify/route/Ring.cfg
```

Run the configs the change touches, one at a time. Nightly runs every config (`just test tla rs/moq-net/verify/route`), simulating each one with a `Simulate:` line and expecting `Loop.cfg` to break. Times are on the recipe's 4 workers. `Patient` is TRUE unless the table says otherwise.

| Config | Mesh | Bounds | Distinct states | Run |
| --- | --- | --- | --- | --- |
| `RouteHandoff.cfg` | The publisher behind relay 1, in a triangle with relays 2 and 3. | 1 claim change, 1 session change | 407,240 | a minute or two |
| `Early.cfg` | The same, with a timer that fires at any moment. | 1 claim change, 1 session change | 4,178,590 | ten minutes |
| `Twice.cfg` | The same, where two sessions close, as when relay 1 dies. Simulated: `num` counts behaviours per worker. | 1 claim change, 2 session changes | 40,000 behaviours, 4.0M states | a few minutes |
| `Ring.cfg` | The publisher and three relays in a ring, with a timer that fires at any moment. | 1 claim change, 1 session change | 426,409 | a minute or two |
| `Lag.cfg` | Relay 3 hears the publisher through relays 1 and 2: the lagging neighbour. | 2 claim changes, 1 session change | 1,299,405 | a few minutes |
| `LagWide.cfg` | The same, with relay 1 asking for `"a"` alone. | 2 claim changes, 1 session change | 4,194,747 | ten minutes |
| `Dead.cfg` | Two relays behind a relay whose only uplink asks for `"a"` alone, one with a dear session of its own to the publisher. Simulated. | 3 claim changes, 1 session change | 40,000 behaviours, 4.8M states | ten minutes |
| `Blind.cfg` | A relay whose nearest neighbour asks for `"a"` alone, and a farther one knows everything. Simulated. | 2 claim changes, 1 session change | 40,000 behaviours, 4.8M states | ten minutes |
| `Loop.cfg` | The publisher with a session to each of three relays in a triangle, all closing. Open finding: `RequestsLoopFree` breaks. | 3 session changes | 979,474 when it breaks | a few minutes |

Run them last, one at a time. First run what answers in seconds: the prototype's tests (`just rs test -p moq-net --lib -E 'test(/route::tests::(paths|compare|copies)/)'`), then TLC's simulation mode with raised bounds in a copy of a config, such as `-simulate num=3000 -depth 150`.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the prototype with `PathSim` (`deliver`, `disconnect`, `tick`), one call per step, except a timer. `Lift(n, x)` fires one timer on one relay, where `tick` moves every running relay's clock by one hop of delay: call it until that doubt lifts, and where another doubt lifted with it, the replay took a different path, which is no defect in the model.

## Coverage

Sync commit: `d3b6c2573a30541402d396f8080488646f673594`. Run `git log d3b6c2573a.. -- rs/moq-net/src/route/tests/paths.rs` to see what changed since. The model describes the prototype, extended with a route per prefix and with dead hops kept until their timer fires; `route::Router` does not run this rule yet. `Relay::collect` forgets a dead hop as soon as no route names it, so `NoStaleReturn` covers the rule the router is to run, not the prototype's. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/route/tests/paths.rs` `Relay::select`, `Relay::pending`, `Relay::hopes`, `Relay::doubted` | `SelectKey`, `Pending`, `Hopes`, `Doubted` |
| `rs/moq-net/src/route/tests/paths.rs` `Relay::want`, `Relay::advertise`, `Relay::report` | `Want`, `Advertise`, `Report` |
| `rs/moq-net/src/route/tests/paths.rs` `Relay::recv`, `Relay::fail`, `Relay::disconnect`, `Relay::connect`, `Relay::change` | `Recv`, `Fail`, `Disconnect`, `Connect`, `ChangeClaim` |
| `rs/moq-net/src/route/tests/paths.rs` `Relay::collect`, `Relay::tick` | `Collect`, `Lift` |
| `rs/moq-net/src/route/tests/paths.rs` `Relay::show` | `Know`, `ShownNow`, `Events` |
| `rs/moq-net/src/route/tests/paths.rs` `PathSim::deliver` | `Deliver` |

## Not covered

One publisher, so routes for several publishers, their copies (`Relay::warm`, `Relay::source`), and the copy rank are left out; the DEAD message's per-publisher batching reduces to one route per prefix. Two prefixes, one nested in the other, and a scoped relay asks every neighbour for the narrow one: no wildcards, interest edits, grants, qualified keys, or hidden claims. Leaves out restarts, GOAWAY, priorities, costs other than 1 and 2, RTT, and the hop limit. A claims message carries the sender's whole view for its prefix, equal to applying its changes on a FIFO stream. A route is sent on every stream whether or not the publisher has a claim there. The publisher ignores what it hears. No exhaustive pass has more than four relays or two session changes; the prototype's sweeps in `compare.rs` cover larger meshes and churn.
