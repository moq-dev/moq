# [XL] The new route rule is measured against today's

## Goal

A test-only prototype of the [epic](/quest/m2/multicast/README.md)'s route
rule runs beside `route::path::Router` on the same meshes and failure
schedules, and the PR reports for both what each requirement costs: messages
and retained state as forwarding nodes, publishers, broadcasts, and links
grow; the time to a settled route and the time a reader goes unfed after each
kind of failure; and every loop, stale route, false END, and revival seen.
The maintainer picks from those numbers whether the rule goes ahead as
decided and with what changes, and the PR amends the
[router](/quest/m2/multicast/router.md),
[switch](/quest/m2/multicast/switch.md), and
[proof](/quest/m2/multicast/proof.md) quests with what it learned.

## Plan

Decided 2026-11-15 (maintainer, /quest-plan): the rule's direction is settled
in the epic (items 3, 4, and 6); this measures it before anything is built on
it, as [moq-dev/moq#8533](https://github.com/moq-dev/moq/pull/8533) measured
paths against Babel.

Build on `rs/moq-net/src/route/tests/`: `Sim` drives a router over a mesh
with messages held and delivered out of step, `compare.rs` ran two routers
over one schedule, and `search.rs` walks every delivery order on a small
mesh. The prototype stays under `tests/`; nothing ships from this PR.

The rule to prototype:

- A route per forwarding node, ranked not draining, then the lowest priority
  on its path, then cost. A node never uses a route whose path names it.
- A closed session is named once per neighbour, and every node drops every
  route through that hop. No moved list, no hold, no doubt, and a dead hop
  is forgotten once no heard route names it.
- An endpoint's claims belong to the relay it connects to. A claim crossing
  a scoped grant is announced again with the chain of the epic's item 3.
- One mark per link, as `upstream` is today.
- A request is confirmed only once its upstream is, and one fed around a
  loop reports no success. Count what that costs readers: this is the
  accepted price of having no timer.

Shapes, each at more than one size:

- CDN tiers: edges on two cores per region with the cores meshed, up to at
  least 32 relays and 123 links at diameter 4.
- Meshes: a ring with log N chords, a sparse random mesh, a clique, and a
  mesh with leaves that mark every link.
- Two meshes joined by one scoped link and by two, and three meshes in a
  chain and in a ring, where the chain must stop a claim circling.
- One broadcast per publisher, the case where routes per publisher helped
  least.

Failures: a clean close; a silent death its neighbours notice up to the idle
timeout apart; a relay with many neighbours dying; a link that flaps faster
than any timer; a publisher on two relays losing one, then both; a publisher
moving; a GOAWAY drain; a partition that heals; and unrelated failures at the
same instant.

Today's numbers, to beat or to explain:

| Case | Today |
| --- | --- |
| A relay with several neighbours dies, 24 relays | 124 route messages, no loop |
| A link closes in a clique, one publisher behind it, then eight | 44, then 198 messages |
| A detour, settled | 12 hops of delay, 6.2 avoiding the far end |
| A publisher moves to another relay, clique of 24 | 1,100 ms dark |
| Churn, 120,000 steps | 32 steps with a route loop |
| A silent death noticed over 3 s | three relays each move onto a dead route twice |
| A link comes up on a 32-relay full mesh | 592 route messages |
| One route change beside 1,000 unrelated streams | 1.09 ms, against 9.8 µs beside 10 |

The known risk is the reason doubt exists. Paths with no dead-hop message
hunted for 65,219 messages when a publisher left a clique of 12, and with the
dead hop but no doubt, selected routes looped when a relay with several
neighbours died. The epic accepts a loop among selected routes while
requests fail closed; show what it costs in messages and unfed readers. If
that is worse than today, try evidence before any timer: for one, rank every
route through the far end of a closed session as draining until that node's
own word about the session arrives, which reuses the drain and needs no
clock. A timer in the rule goes back to the maintainer.

Keep the gate fast: the fixtures that pin a property run in the merge gate,
and the long sweeps run nightly from `test/route-sweep.sh`.

Public API: none. Wire: none.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - where today's numbers were measured
