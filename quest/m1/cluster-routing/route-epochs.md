# [L] Mint an epoch for every route

## Goal

Every route has an epoch: the receiving side mints one at ingest or local
announce when a peer sends none, so `Instance` collapses to `Epoch`.
`Instance::Route(generation)` and its renewals, `restarted_since`, the
held-restart Release for epochless routes, and Selection retirement are
gone, leaving Follow's same-epoch Update/Restart split as the only takeover
rule. No wire change.

## Plan

Decided 2026-10-27 (maintainer): this was the 2026-10-24 pre-cut audit's
proposal in [Routes and announces](/quest/m1/cluster-routing/routes.md), and
is now decided rather than a proposal for routes to weigh. It is a ready
quest of its own, and routes and
[announcement takeover](/quest/m1/announce-takeover.md) both build on it.

The epochless instance machinery keeps drawing fixes (35d935f428,
3655358c2f, c39780f952, 29e74a26b7, a7acf21e2a, ef2511e101, aa0d915867,
0f4f81f9ed, f0fcbefd66, 4c8ab3ca41, 001482144f, 07127f8f5b), as does front
splice, park, and linger in `serve_front` (166f9e87bc, 3b4ad5bf02,
88848954db, 24b9177b6e, 0e1995017d, 732101ed1e, 00d27780dd, 37b49d8042).
Most of it lives in `rs/moq-net/src/model/origin.rs`.

- Watch that a minted epoch never makes an epochless change of serving route
  look like continuity: routes keeps that a Restart, and the routing
  contract's epochless fixtures
  (`rs/moq-net/tests/integration/routing_contracts/`) keep passing.
- Check whether `js/net` keeps a matching route generation, and mirror the
  change there if it does.

Public API: `Route::epoch` stops being optional (breaking), and the Impact
bullet says how to migrate. Wire: none.

Open (2026-10-27, implementation): a minted epoch cannot stand in for a
declared one under today's best-route-per-prefix advertisement. Minting at
table insert and treating it as an epoch fails 18 `moq-net` lib tests. Each
node mints at its own time, so "newest epoch wins" lets mint order outrank
cost, preference, and the local-broadcast tie, and nodes disagree on a pool
path's winner (`equal_cost_pool_spreads_paths`,
`route_changes_preserve_each_cursors_visible_winner`,
`local_broadcast_wins_a_tie_with_a_hopless_route`), contradicting
`pool_ties_agree_only_on_a_path_and_identity_hash`. Ranking a minted epoch
last keeps the spread, but then a pool path's winner differs from the
prefix winner whose epoch is advertised. Forwarding that epoch makes a
downstream pinned request refuse the path, or resume across a worker change.
Keeping it local makes a downstream relay's own mint resume across the same
change unless the prefix restarts first, which is the renewal and held
Release this quest deletes. Every check that today asks "has an epoch"
would ask "was it declared", so nothing collapses. Under
[Routes and announces](/quest/m1/cluster-routing/routes.md), each origin is
its own ANNOUNCE and the first hop pins the request to its identity, so an
epochless instance is the first hop's route and the per-hop renewal has
nothing left to protect. Decide whether to fold this into routes and
unblock [announcement takeover](/quest/m1/announce-takeover.md), which does
not need minting.
