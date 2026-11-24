---
name: verify-warm
description: Model-check warm-pull routing convergence (`rs/moq-net/verify/warm/WarmSettle.tla`) with TLA+. Use when changing relay route/warm-copy selection, warm adverts, or neighbor ranking in `rs/moq-net`.
---

# Verify warm pulls settle

`rs/moq-net/verify/warm/WarmSettle.tla` models where each relay pulls one broadcast from: its route's next hop, or a neighbour with a warm copy it heard of. What one relay tells a neighbour over a session (the rank its pull carries, its answer to the neighbour's pull, and its advert) is a register the neighbour reads late, so every relay acts on stale words for as long as TLC likes. Readers start and stop and uplinks close and reopen up to a bound; then nothing outside changes. With weak fairness on every delivery and every relay's reaction, TLC checks that the pulls then go quiet and every reader with a route is fed, from every state the bounded outside events reach.

`Rule` picks the rule:

- `"order"`: ranked readers, the rule [moq-dev/moq#8986](https://github.com/moq-dev/moq/pull/8986) measured and `quest/m3/cache-routing/select.md` ports. A reader ranks by its relay's route cost, then its relay (`Order = "route"`), or by relay alone (`"id"`). A relay takes a warm copy only from a neighbour feeding a reader ranked below every reader it feeds itself, priced at the link's cold price alone, and heads for the cheapest such copy or its route whenever either changes. A pull carries the lowest rank it feeds, and an advert or answer the lowest rank its sender feeds besides its receiver.
- `"orderless"`: the same with the rank check removed.
- `"pick"`: the price-moving pick moq-dev/moq#8903 rejected.
- `"stay"`: settled pulls, which the maintainer rejected for ranked readers on 2026-11-17; kept to show the model covers both.

## Rules

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `TypeOK` | Every pull and move names a neighbour, a move never names the pull it replaces, and only a pull or move is remembered as answered. | The model, not the rule, is wrong. |
| `PullsQuiet` | Once the outside events stop, the pulls stop changing (`<>[][UNCHANGED <<up, nxt>>]_vars`), however the registers keep moving. | Relays move pulls between copies for good, as case 503 did under the pick: copies flap and readers stall on every move. |
| `EventuallyFed` | Once the outside events stop, every reader whose relay has a route is fed for good: its pulls lead to the publisher's relay, each held by its parent and answered live. | Two relays wait on each other, or a reader with a route is never fed. |

## Run

```sh
quest gate just rs tla moq-net/verify/warm/WarmSettle.tla -config rs/moq-net/verify/warm/Drones.cfg
```

| Config | Rule | Checks | States, time at four workers |
| --- | --- | --- | --- |
| `Drones.cfg` | ranked by route cost | three drones meshed with costly uplinks, readers starting and stopping three times | 158,198, 5 minutes |
| `Hash.cfg` | ranked by relay | the same | 158,198, 5 minutes |
| `Flap.cfg` | ranked by route cost | readers fixed at two drones while one's uplink closes and reopens, changing its rank | 44,427, under a minute |
| `Mesh503.cfg` | ranked by route cost | churned random mesh 503's six relays and readers, from no pulls | 490,125, 20 minutes |
| `Settled.cfg` | settled pulls | the same | 407,526, 6 minutes |
| `SettledDrones.cfg` | settled pulls | as `Drones.cfg` | 6,852, seconds |
| `Pick.cfg` | the price-moving pick | as `Mesh503.cfg`; fails `PullsQuiet` (open finding) | fails within 6 minutes |
| `Orderless.cfg` | ranked, rank check removed | as `Drones.cfg`; fails `PullsQuiet` (open finding) | fails within 9 minutes |

Nightly runs every config here and fails if a config with an `\* Open finding:` line passes. Two carry one, and neither is to be fixed. `Pick.cfg` shows the check has teeth: the rule it models is the one moq-dev/moq#8903 rejected for never settling case 503. `Orderless.cfg` shows the rank is what settles the ranked rule: without it, readers at two drones each take the other's warm copy over their uplinks, find their own path in what they now pull, drop it, and repeat for good.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the simulator: build the shape with `WarmSim::link` and `publish`, start readers with `read`, close links with `close`, and deliver one message at a time with `deliver` in the model's order, under the arm the config's `Rule` names (`ORDER_ROUTE`, `ORDER_HASH`, `REROUTE_UNQUERIED`, `NOT_DOWN_QUERY` in `route/tests/warmth.rs`). `ranked_pulls_settle_random_case_503` replays case 503.

## Coverage

Sync commit: `1399add3c886f1c168f0c6ba3ba55586a04feb6c`, the base of [moq-dev/moq#8986](https://github.com/moq-dev/moq/pull/8986), which added the ranked rule and this model. Run `git log 1399add3c8.. -- rs/moq-net/src/route/tests/warm.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function. The rule lives in the simulator only; the port in `select.md` moves it into the relay and this table with it.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/route/tests/warm.rs` `WarmSim::ordered`, `pull`, `toward` | `Decide` (`Ordered`), `Next1`, `Toward` |
| `rs/moq-net/src/route/tests/warm.rs` `WarmSim::options`, `copy`, `best` | `Options`, `Heard`, `CopyCost`, `Best`, `Better` (relay IDs for rendezvous hashes) |
| `rs/moq-net/src/route/tests/warm.rs` `WarmSim::rank`, `own`, `ranked` | `RankOf`, `Own`, `Pub` (`pull`) |
| `rs/moq-net/src/route/tests/warm.rs` `WarmSim::offer`, `warm`, `answer`, `advertise`, `scoped` | `Offer`, `Answer`, `Advert`, `Useful` |
| `rs/moq-net/src/route/tests/warm.rs` `WarmSim::stay` | `Decide` (`"stay"`), `Chose` |
| `rs/moq-net/src/route/tests/warm.rs` `WarmSim::close`, `reopen` | `Close`, `Reopen` |

## Not covered

One broadcast and one publisher, which never moves. The route layer is an oracle: each relay's route is the shortest over the open links at once, the lowest relay among equal ones; `verify-route` and the route proof cover how routes settle. The three words one session carries one way arrive together, though the subscription, its answer, and the advert ride separate streams, and a register keeps only its newest value, so a relay may skip values but never reads one out of order on one session. No link is marked upstream, so the mark is left out, and there is no query. A pull replaced by one through the same neighbour is the same pull. Relays never restart. A pass covers four relays with three reader changes, or two fixed readers and one uplink closing and reopening, and case 503's six relays with their readers fixed. A reader change during the flap passed 2M states without finishing, so it is left out.
