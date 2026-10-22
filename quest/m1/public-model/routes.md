# [M] Public routes

## Goal

A custom relay or session does what the built-in ones do with routes,
through public moq-net and @moq/net calls only: ingest a peer's routes under
the peer's identity, and advertise routes to a peer without echoing the
peer's own routes back.

## Plan

Split out of [Public answering](/quest/m1/public-model/answer.md) on
2026-10-22 (maintainer, from an external API audit): every item below hangs
off the `Hop`/`Cost` route model that [Routes and
announces](/quest/m1/cluster-routing/routes.md) replaces with node ids,
seqnos, and a down-only metric in this same release. Publishing them first
would freeze machinery already scheduled for removal, so this waits for that
quest and then reruns the inventory against the route layer it lands.

The items and decisions below are as they stood at b4eab0db1. Keep each
decision's intent: the announcing identity lives on the origin handle and a
`Route` can never forward it, a publisher advertises split-horizon from a
consumer that excludes the peer, and the peer-route escape hatches go
private once a peer's route has a public shape. Expose the roles that survive
the route layer under their new types, and drop items whose need it removes
(for example `Hop::from_wire` and `Cost::charged` if `Hops` and saturating
`Cost` are gone). Rules, carve-outs, and the inventory method are in the
[epic](/quest/m1/public-model/README.md).

- `origin::Hop::from_wire` (IETF cluster): public as the decode-side
  constructor that accepts 0 (`Hop::UNKNOWN`), beside `Hop::new`.
- `origin::Cost::charged` (both subscribers, IETF cluster): public.
  `Cost::UNKNOWN` goes; it equals `Cost::default()`.
- **Announcing identity** (`origin::Route::via`, `Route::source`,
  `Route::with_via`; both subscribers, server). **Decision**:
  - ✅ Replace. The identity moves onto the origin handle a session
    announces through, as `origin::Producer::via(hop)` beside the public
    `Producer::peer` (the name `via` stays), so every route it ingests
    carries it and a `Route` can never forward it. `Route.via` and
    `Route.source` stay private fields the handle sets.
  - A public `Route::with_via`, which a caller could forward by mistake.
- `origin::Consumer::excluding` (both publishers, client): public;
  split-horizon toward a peer, the counterpart of the identity above.
- **Peer-route escape hatches** (`origin::Producer::dynamic_route`,
  `origin::Dynamic::update_route`, `origin::Request::accept`; JS
  `Origin.Producer.dynamicRoute`, `Dynamic.updateRoute`, `Request.accept`).
  Settled as the interim in [#5678](https://github.com/moq-dev/moq/pull/5678):
  an application claim (`dynamic(prefix)`) carries no route and answers
  through `Request::prepare` then `Prepared::publish`, and these stay
  `#[doc(hidden)] pub` / `@internal` for the sessions, tests, and benches that
  model a peer's route (a covering claim with hops, cost, or an epoch,
  answered with a broadcast the session already routes). Make them private
  (`pub(crate)`, and private in JS) once the identity handle above and the
  answer source give a peer's route a public shape, and move the
  out-of-crate tests and benches onto it.
- `wire.ts` `OriginProducer.receive` (`connection/forward.ts`): mirror the
  identity decision. A received route is a public `Origin.Producer.dynamic`
  on a handle marked with the peer's identity, so `receive` goes and
  `Origin.Request.accept` stops special-casing session-delivered broadcasts
  (`Broadcast.fromSession`).
- `wire.ts` `OriginConsumer.advertised`, `local`, `demand`, with
  `Advertised`, `Advertisements`, `sameInstance`, and `internal.ts`
  `presented`, `hiddenBelow`, `scopeHead`, `scopeCaptures`, `scopeOverlaps`
  (both publishers and subscribers): replace with the Rust shape. A
  publisher advertises from the public `announced()` stream of an
  `excluding(peer)` consumer and resolves with the public request taking an
  epoch, as Rust does. The helpers stay private to `origin.ts`. Check that
  the public announce event carries what `sameInstance` and a scoped
  route's claim need; if it does not, that is track metadata and goes public.
- `hop.ts` `HopSchema`, `randomHop`, `UNKNOWN_HOP`, `MAX_HOPS`,
  `Cost.zero`, `Route.default`, `Route.normalize`, `routesEqual`: public
  values under `Origin`, mirroring Rust's `Hop::new`, `Hop::UNKNOWN`,
  `Route::default` and `PartialEq`. `Hop` is branded, so a third party
  cannot build `Route.hops` without a cast today. Today `Route.default` is
  one shared mutable object (`{ hops: [], cost: 0n }`), so a caller's
  `hops.push` would pollute every later route. Publish it as a factory
  returning a fresh route (`Route.empty()`, like Rust's `Route::default()`),
  and move the internal readers onto it, rather than exporting the shared
  object.

Tests: each public call gets a mock-time test in its language, and the
built-in sessions move onto the public calls, so their existing suites
cover the behavior.

## Required

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - replaces the hop and cost model these items expose
- [Public answering](/quest/m1/public-model/answer.md) - the answer source the escape hatches' replacement builds on
