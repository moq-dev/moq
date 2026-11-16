# [XL] lite-07 sessions run the new route rule

## Goal

Every lite-07 session in Rust and `js/net` drives the
[new router](/quest/m2/multicast/router.md) on one announce stream per side,
the draft specifies it, and the old rule is gone: ROUTE_HOLD, the doubt,
DEAD's moved list, the Route Key Parameter with its proof and symbolic model,
qualified Node IDs, routes per prefix with their Route Suffix and Route
Depth, `path::Router`, and `RouteHandoff.tla`. The draft's routing text is
shorter than today's and the announce stream carries fewer message types.

## Plan

Decided 2026-11-15 (maintainer, /quest-plan): lite-07 carries the new rule
and the cut waits for it ([epic](/quest/m2/multicast/README.md) item 2).
lite-07 is `-wip`, so relays pinned to `main` roll together, as each earlier
lite-07 wire change did.

Recommended wire, for the implementer to confirm or improve; each departure
is a decision to list in the PR:

- A route is started, updated, and ended, and a closed session is named in
  one fixed-size message. Nothing answers a hold, and no message lists moved
  routes: the updates follow in order on the same stream.
- A claim carries its chain as a field of its own, replacing the temporary
  Lite-06 Chain and its Setup Parameter: the nodes that announced it again
  across a scoped grant or an older version.
- A request keeps its Instance and the origin node its first hop chose, and
  loses Route Depth. That ends the second subscription a move from a claim's
  route to its answer's opens on one link, which
  `route_failover::residual_two_depths_of_one_claim_subscribe_twice` pins
  ([moq-dev/moq#8881](https://github.com/moq-dev/moq/pull/8881)): flip it to
  one subscription.
- An endpoint that never forwards declares no Node ID. A peer holding a
  scoped publish grant sends claims and no routes, and the relay announces
  them under its own Node ID. Peers that trust each other with the whole
  tree exchange routes.
- The link mark stays as `upstream`. moq-net's per-link `proxy` switch, which
  the relay hard-wires on, goes if nothing else needs it; that is a public
  API change to report.

Delete with the old rule:

- `rs/moq-net/src/route/proof.rs`, `js/net/src/lite/route_key.ts`,
  `rs/moq-net/verify/route-key/`, the `verify-route-key` skill, and the
  interop route key lane (`test/interop/clients/js/route-key.ts`).
- `rs/moq-net/verify/route/` and the `verify-route` skill, or rewrite the
  skill for the exhaustive search.
- [Firefox tabs prove their route key](/quest/m5/routes-browser-proof.md),
  and the release cut's reason to wait on
  [the Playwright bump](/quest/m1/playwright-pin.md).
- [Doubt holds](/quest/m2/cluster-routes/doubt-holds.md),
  [doubt and keep-alive](/quest/m2/cluster-routes/doubt-keepalive.md),
  [held after close](/quest/m2/cluster-routes/held-after-close.md), and
  whatever of [route fan-out](/quest/m2/cluster-routes/route-fanout.md) the
  new router answers, with the fixtures that pin them.

Rewrite the assertions of the acceptance fixtures
(`tests/integration/route_acceptance.rs`, `route_failover.rs`,
`route_cluster.rs`) for the new rule, keeping their topologies.

Quests written against the old wire, to retarget or delete in this PR:
[outbound federation](/quest/m3/cluster-routing/federation.md),
[the routing guide](/quest/m3/routing-guide.md),
[route wakes](/quest/m4/route-wakes.md),
[handoff routes](/quest/m4/relay-handoff-routes.md),
[regrant recheck](/quest/m4/regrant-route-recheck.md),
[announce interest memory](/quest/m4/announce-interest-memory.md),
[the mesh condition](/quest/m5/lite07-mesh.md), and
[route memory](/quest/m9/perf/cluster-route-memory.md).

Update the draft's Announce, Floors, Routing, Dead Hops, and Resolution
sections, the message sections, the Setup Parameters, and the lite-07
changelog, and `doc/bin/relay/cluster.md`. Run `just check`,
`just test interop --all`, and `just drafts check`.

Public API: the per-link `proxy` switch, if removed. Wire: lite-07-wip only.

## Required

- [The router](/quest/m2/multicast/router.md) - the rule these sessions drive
- [The route stream](/quest/m2/cluster-routes/route-stream.md) - one announce stream per side, which this builds on
