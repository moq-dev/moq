# When a node may forget a feasibility distance: model results

Reference only, from 2026-11-10. `RouteHandoff.tla` at this commit has three
switches, and this records what TLC found for each. Nothing here is decided;
the choice between the deferred echo and the hop list is the maintainer's.

- `Echo`: `"once"` echoes a ROUTE_END as soon as it is applied (today's rule),
  `"transitive"` defers it, `"none"` sends no echo.
- `EchoLane`: `TRUE` lets an echo overtake its sender's other messages, as it
  can on today's wire. `FALSE` is one FIFO per direction.
- `Hops`: `TRUE` makes each route name the nodes it crossed. A node keeps a
  route that names it until it ends and never selects it, counts it as
  infeasible, or lets it certify.

Bounds are claim changes / flaps / collects. Shapes: the triangle has the
origin as one of its nodes. "Behind" is origin 0 on relay 1 of the triangle
1, 2, 3, with unit costs or with `{0,1}=1, {1,2}=2, {1,3}=1, {2,3}=2`.

## The deferred echo (`Echo = "transitive"`)

Three sub-rules, each forced by a counterexample:

1. A node defers the echo when it selected the sender's route, or selects
   nothing and holds on the sender's held one. Selection alone missed a held
   route and counted to infinity.
2. While the node holds its route the echo stays undecided. Echoing at once
   let the upstream forget while a copy was still in flight behind the HOLD.
3. Once decided, the echo waits on the ROUTE_ENDs outstanding at that moment
   and none sent later. Waiting on all of them deadlocked two relays that
   had each routed through the other at different times.

| Shape | Bounds | Result | Distinct states |
|---|---|---|---|
| Triangle | 2/2/1 | pass | 811,197 |
| Behind, unequal | 1/1/0 | pass | 536,339 |
| Behind, unequal | 2/1/0 | pass | 4,504,498 |
| Behind, unequal | 1/1/1 | pass | 1,846,941 |
| Behind, unequal | 1/2/0 | **NoCountToInfinity fails**, 23 states | |
| Behind, unequal, metric bound 11 in place of the rule | 1/2/0 | pass | 5,639,484 |
| Behind, unit | 1/1/0 | pass | 305,446 |
| Behind, unit | 2/1/0 | pass | 3,031,809 |
| Behind, unit | 1/2/0 | pass | 2,054,297 |
| Behind, three other cost assignments | 1/1/0 | pass | 496,590, 488,878, 493,323 |
| Behind, relay 3 a leaf | 1/1/0 | pass | 606 |
| Ring of four, origin a member, unit and one costly link | 1/1/0 | pass | 17,734, 14,731 |
| Line of four | 1/1/0 | pass | 460 |
| Two attachments (0-1, 0-2, triangle 1, 2, 3), unit | 1/1/0 | stopped at 13.9M, no violation | |
| Two attachments, unequal | 1/1/0 | stopped at 21.5M, no violation | |

The failure: the origin's link closes, then the session between relays 1 and
3. Closing counts as relay 3's echo, but relay 3's last advertisement is
still in flight to relay 2, which hands it back to relay 1 after relay 1 has
forgotten. `DrainCostsTransitive.cfg` reproduces it. The stale route stops at
relay 2, which still remembers; with the metric bound at 11 (two laps) every
other rule holds.

With `Echo = "once"` and no hop list, the triangle reproduces the baseline
(1,227,926 states at 2/2/1) and the behind shapes fail in 20 and 23 states.

## The hop list (`Hops = TRUE`, `Echo = "once"`)

| Shape | Bounds | Result | Distinct states |
|---|---|---|---|
| Triangle | 2/2/1 | pass | 485,972 |
| Behind, unequal | 1/1/0 | pass | 16,314 |
| Behind, unequal | 1/2/0 | pass | 592,146 |
| Behind, unit | 1/2/0 | pass | 840,046 |
| Behind, three other cost assignments | 1/2/0 | pass | 879,800, 582,283, 879,800 |
| Behind, relay 3 a leaf | 1/2/0 | pass | 2,526 |
| Line of four | 2/2/1 | pass | 83,540 |
| Ring of four, origin a member, unit | 2/2/1 | stopped at 18.5M, no violation | |
| Ring of four, one costly link | 2/2/1 | stopped at 8.3M, no violation | |

The echo is still needed: with `Echo = "none"` a node forgets while a
neighbor still routes through its advertisement, and `LoopFree` fails in 944
states on the behind shape.

Dropping a self-naming route outright, instead of keeping it as an unusable
entry, let `collect` run while the neighbor's route was still open and failed
`QuietIsSettled` on the triangle.

## The hop list with unordered echoes (`EchoLane = TRUE`)

| Shape | Bounds | Result | Distinct states |
|---|---|---|---|
| Triangle | 2/2/1 | pass | 1,594,721 |
| Behind, unequal | 1/1/0 | pass | 92,808 |
| Behind, unequal | 1/2/0 | pass | 3,252,617 |
| Behind, unit | 1/2/0 | pass | 4,619,352 |
| Two attachments, unequal | 1/1/0 | stopped at 11.8M, no violation | |

Without the hop list the same ordering counts to infinity on the triangle in
11 states, which is the bug in moq-dev/moq#8447.

## Not run

- Either variant at 2/2/1 on the behind shapes.
- Five relays (an origin behind a ring of four, and two attachments with a
  ring), and random-cost simulation sweeps.
- The stopped rows above, to completion.

No `Sim` replay of the two-flap trace yet, and neither variant is in the
router.
