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
